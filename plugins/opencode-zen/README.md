# OpenCode Zen（免费层）插件

把 OpenCode Zen 匿名免费层注册为 Zroutery 的对话提供方。声明走插件 API
（`host.protocol.openai_chat` + `host.catalog.openai`），核心不含 zen 专属
分支；会话生成、身份头、环境块与登记全部由宿主的 `opencode_cli` 整形档位
（manifest 的 `shaping` 声明）驱动。

## 打包

```powershell
python plugins\pack.py opencode-zen
```

## 安装与使用

1. 设置 → 插件 → 安装插件包 → `opencode-zen-0.1.0.zrp` → 批准权限。
2. 提供方页 → 添加 OpenCode Zen 连接（无需密钥——匿名免费层）。
3. 模型页从目录拉取（或手动添加 `iceman-free` 等 `-free` 模型）→ 接入分流。

## 门禁满足方式（2026-11-22 现场验证）

| 门禁项 | 满足方式 |
|---|---|
| UA 形状 | 宿主 `opencode_cli` 整形档位发出 `opencode/latest/2.3.2/opencode`（版本需跟随 OpenCode 更新器，旧版被以配额文案拒绝） |
| 会话登记 | 每对话生成规范 `ses_` 会话，首次对话前宿主先 `GET /zen/v1/models` 登记 |
| 环境块 | 宿主在 system 缺 `<env>` 块时按请求上下文补齐（缺失时免费层只给每日小额配额，耗尽即 429） |
| 工具 | 客户端工具目录原样透传（追加/改描述均被接受；无强制工具） |

## 已知边界

- 免费层按 IP 限流，高并发会出现配额样 429，稍后自行恢复。
- 网关升级可能再次收紧 UA 版本校验——宿主整形档位的 UA 常量需跟随
  `opencode.ai/update/api/latest/cli/npm` 的最新版本。
- 本插件与核心原生的 `opencode_zen_free` 提供方等价；原生路径同样受益于
  宿主整形层的 UA/登记/环境块修复。
