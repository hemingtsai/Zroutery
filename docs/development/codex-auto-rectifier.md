# Codex 条件整流与工具兼容

## 来源

移植自 `yidu456/magpie-auto-codex-filter`，固定到
`cd68b072ddf3475203d42661d9cdca3a93212100` 的 `proxy.cjs`，Magpie Auto V10。
MIT 署名和完整许可证位于 `docs/licenses/magpie-auto-codex-filter.txt`。

移植的是请求整流、响应暂存和重试策略，不启动 Node 代理、不接管 Magpie/CC Switch
端口、不复刻其服务控制脚本。Zroutery 已有自己的监听、鉴权、配置、凭据库与客户端接管。

## 请求与工具

- 接受 Codex 的 `additional_tools` developer 输入项，不忽略其工具。
- Responses 上游默认保留原始输入位置、namespace、custom 工具、phase、历史 id、推理密文
  和供应商扩展；只覆盖实际模型、流式选择与输出上限。
- Chat/Anthropic 上游把 namespace 下的函数映射为唯一函数名。custom 工具的自由文本
  通过单个 `input` 字符串参数桥接；回复还原为原始名称、namespace 和 `custom_tool_call`。
- 自由文本调用的 SSE 在暂存完成后还原，绝不把桥接用的 JSON 包装当作可执行输入。
- 上游托管工具、压缩和服务器引用仅允许原生 Responses 路径；其他方言明确拒绝，
  不使用空消息替代上下文。
- `/v1/responses/compact` 原生转发一次，不做重试和历史清理。
- 支持 gzip、deflate、Brotli、zstd 请求及上游响应解压。解压后的请求仍受服务正文上限约束。

## 条件清理

`routing.rectifier.enabled` 和 `routing.rectifier.codex_auto` 同时开启才启用，默认开启。
关闭后恢复普通实时流式路径；但带 namespace/custom 工具的流式请求仍暂存，
以完整还原自由文本参数，此时不执行 Auto 的历史清理和自动重试。
供应商未开启 `portable_history` 时，每条请求的第一尝试保留历史。
显式开启后，转发前删除历史 reasoning、可独立携带正文的 item id 和
`previous_response_id`，移除 `reasoning.encrypted_content` include 并设置 `store=false`。
此供应商开关独立于 Auto，在自动整流关闭、仅为工具桥接暂存时也生效。

NewAPI/上游的 `30013`（`session binding invalidated`）是会话与模型的绑定失效，
不是推理密文验证失败。数字/字符串错误码、包装后的消息和 JSON/SSE 错误统一识别；
开启 `routing.rectifier.enabled` 时，由 Zroutery 刷新上游 session id 并保留整条历史重试，
同一候选绑定最多刷新一次，且占用原有重试额度。即使中转用 HTTP 503 或 HTTP 200
包装，也不重放旧绑定、不触发推理清理、不把该会话问题计入模型断路器。
旧 id 与新 id 按提供商、端点、模型、账号/密钥隔离映射，因此 OpenCode 仍发送原 id
也能延续到有效的新绑定。映射只存 id 和非敏感身份，进程内最多 4096 个别名，闲置 24 小时失效。
普通聊天响应通过 `x-zroutery-session-id` / `x-session-id` 返回新 id；Responses 暂存 SSE
通过标准 `response.metadata.zroutery_session_id` 返回，不新增客户端无法识别的 SSE 类型。
Codex CLI 同步 `session_id`、`x-session-id` 和 `prompt_cache_key`，普通响应也返回原生
`session_id` 头。Claude Code 同步 `x-claude-code-session-id` 及 `metadata.user_id`
中的 JSON `session_id` 或旧版 `user_…_account_…_session_<UUID>` 尾部；设备、账号和
其余元数据不变，普通响应回传原生 Claude 会话头。桥接到 Chat/Responses 时同样同步
编码到 `user` 中的会话信息，不把普通用户标识改成会话 id。
刷新后仍失败或 `routing.rectifier.enabled` 关闭时，明确返回 `session_binding_invalidated`，不无限重试。
已经交付正文/工具调用的实时流不透明重放；原生 compact 只复用已有映射，不重试。

Auto 只在下面的明确错误后执行条件清理，不覆盖显式开启的 `portable_history`：

1. HTTP 400，包含完整两句 Azure resource 归属错误。
2. HTTP 400，明确 `error.code = invalid_encrypted_content`。
3. HTTP 200 SSE `error` / `response.failed` 中的同类错误。

认证、权限、配额/钱包/预算错误优先排除。泛泛的签名、解密错误，以及
401/403/422/429/500/503 中的密文错误不触发清理。

清理整条请求最多一次：删除 history reasoning 项、普通消息/函数/自定义工具的
顶层 item id，`store=false`，从 include 中删 `reasoning.encrypted_content`。
保留正文、图片、phase、参数、结果、call_id、嵌套资源和本轮 reasoning.effort。
后续重试复用清理结果；未开启 `portable_history` 时，下一条客户端请求重新从完整历史开始。

需要清理但依赖 conversation、自动 compaction、已有压缩、item_reference 或其他
未能展开的密文上下文时，先验证整条请求，再明确拒绝。失败的清理不会部分修改正文。
Zroutery 自己存储的 previous_response_id 可以展开；未知的上游引用仅走原生路径，
需要跨账号清理时仍拒绝，不擅自删掉引用。

## 暂存、重试、取消

Responses SSE 完整暂存到 `response.completed` 或 `response.incomplete`。
失败尝试的正文、推理和客户端工具都不交付；只有成功尝试交付。
新生成的成功回复推理保留。每轮暂存上限 64 MiB，超限明确失败、不重放。
内存没有全服务总上限，因此高并发长回复仍可能消耗较多内存。

每条客户端请求合计最多额外 20 次，分类额度不乘起来：

| 类别 | 额外次数 | 退避秒数（到末值后保持） |
| --- | ---: | --- |
| 资源/密文 | 5 | 1、2、4、8、15 |
| 临时限流 | 20 | 10、20、40、60、60；另加 0–999ms 抖动 |
| 断流/超时/明确 ALB HTML400 | 10 | 1、2、4、8、15 |
| 无可用渠道 | 10 | 5、10、20、30、60 |

遵守 Retry-After、retry-after-ms、x-ms-retry-after-ms 中最大的等待下限。
总期限 30 分钟，含重试等待；若等待会超期限，不清理、不提前重试。
单轮空闲上限 120 秒（供应商更短时取更短值）。
SSE 等待期间每 15 秒发空进度帧。客户端断开或 cancel 会停止当前发送、退避与后续重试。
`x-zroutery-response-id` 响应头给出可取消的本地 response id，无需等待第一段正文。
原有命名整流规则仍保留，重试也占用同一总额度；供应商失败转移仍受 Zroutery 路由上限约束。
沿用 Zroutery 的日志缓冲/落盘方式，不复制 Node 服务的日志窗口、24 小时文件轮换或
CMD 启停脚本。WebSocket 未加入整流，使用 HTTP/SSE。

鉴权后的 `GET /v1/status` 的 `codex_filter` 包含开关、V10 策略上限和进程累计统计：
活动请求、清理次数、移除 reasoning/id、拒绝清理、分类重试/恢复/耗尽、成功/失败/取消。
数组顺序由 `retry_kinds` 明确给出；不保留任何正文或凭据。

## 验证与限制

离线用例：`codex_auto_test.rs`、`codex_tools_test.rs`、`codex_auto_e2e_test.rs`。
只用临时配置和本地模拟上游，不读真实凭据、不请求收费模型。

相比原 Node 转发器，Zroutery 保持现有错误正文脱敏和账本边界，不把上游 HTML、
账号提示或内部地址原样交付给客户端。日志只记录错误分类、次数和移除计数。
上游托管工具仍可能在失败尝试中执行并计费；客户端工具输出被暂存保护不代表
上游执行能够撤销。此整流不能保证所有账号资源或密文错误都能恢复。
