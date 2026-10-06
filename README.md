# Zroutery

把多个 LLM provider 聚合成**一个**本地端点，对外同时支持四种 API 方言。除了真实模型 id，还额外暴露
`fast-class` / `standard-class` / `reasoning-class` 三个虚拟模型（名字随层级命名设置变化），
由后端按你手动指定的级别选模型。

macOS 桌面应用：常驻菜单栏，无 Dock 图标，关窗不退出。

```
客户端 ──┬─ POST /v1/messages           (Anthropic 方言)
         ├─ POST /v1/chat/completions   (OpenAI 方言)
         ├─ POST /v1/responses          (OpenAI Responses 方言)
         └─ POST /v1/generateContent    (Gemini 方言)
                    │
              统一 IR + 路由 + 失败转移
                    │
         ┌──────────┴──────────┐
    DeepSeek (OpenAI 兼容)   OpenAI / Anthropic / Ollama / vLLM …
```

## 快速开始

前置：Rust 1.89+、Node 20.19+（或 22.12+）、pnpm、Xcode Command Line Tools。

```sh
pnpm install                # tauri CLI 和前端依赖
pnpm dev                    # 开发模式（热更新）
pnpm build                  # 产出 target/release/bundle/{macos,dmg}
pnpm test                   # cargo test --workspace
pnpm smoke                  # 假 provider 端到端冒烟（方言、流式、计费、选举）
pnpm test:layout            # 无头 Chromium 量真实界面布局
```

> `pnpm build` 的 DMG 步骤用 `hdiutil` + Finder，需要正常桌面会话；只要 `.app` 用
> `pnpm tauri build --bundles app`。产物约 13MB，内含 `zroutery-headless`，可直接从 bundle 启动无界面代理。

首次运行会在 `~/Library/Application Support/app.zroutery.desktop/config.json` 生成配置和本地 token；
API key 存 macOS 钥匙串，不落配置文件。无图形环境可只跑代理：

```sh
cargo run -p zroutery --bin zroutery-headless
# 可选：ZROUTERY_CONFIG_DIR=/path/to/dir  ZROUTERY_KEY_PROVIDER_DEEPSEEK=sk-xxx
```

## 配置三步

1. **Providers**：添加 provider（OpenAI 兼容 / Anthropic），填 base URL 和 API key；
   “Fetch models” 拉取上游模型列表，可顺便选余额探测预设。
2. **Models**：给每个模型选 class。**级别永远由你指定，程序不猜**；没选级别的模型只能用精确 id 调用，
   不参与 `*-class` 路由。
3. **Routing**：类内策略、失败转移次数、熔断阈值、预算护栏、分类器池。

### 类内策略

| 策略 | 行为 |
| --- | --- |
| Balanced（选举） | 按实测延迟 + 价格打分排序，见下 |
| Priority | 优先级数字小的先用，同级按权重随机 |
| Weighted random | 按权重随机分摊 |
| Round robin | 轮流，忽略优先级 |
| Lowest latency | 历史延迟最低者优先 |

选举（Balanced）靠一轮 **1 token** 探测请求量延迟，结合价格打分后**排好序钉住**，不是每请求重算：

- 启动时跑一次（可关）、界面点 **Re-run now**、或 `zroutery-headless --elect`；
- 每个轴按「是类内最好的几倍」算分（1.0 为最好，100 倍封顶），免费 vs 收费不会除零；
- 价格只有在所有成员都有价且币种一致时才参与，否则退化成只看延迟并**把原因写在界面上**；
- 探测失败的模型排最后并留下错误；平手退回手填优先级再退回 id，保证结果可复现；
- 探测本身也是最新健康信号，会记进健康表。

### 模型 id 规则

模型身份是 `(provider, 上游模型名)`，对外 id 由此推导：`<provider>-<模型名>`，不单独存储：

```
deepseek + deepseek-v4-pro    →  deepseek-deepseek-v4-pro
openrouter + deepseek/r1:free →  openrouter-deepseek-r1-free   (/ 和 : 变成 -)
```

发给上游的仍是原始模型名；嫌长可在模型详情加 aliases。0.1.x 升级时旧手写 `id` 自动变成 alias。

虚拟模型名由 `routing.naming_style` 决定：`internal`（默认）、`anthropic`（`haiku/sonnet/opus-class`）、
`openai`（`luna/terra/sol-class`）；另外两套名字仍可解析，模型列表只显示当前套。

## 接客户端

```sh
# Anthropic 风格（含 Claude Code）
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
export ANTHROPIC_AUTH_TOKEN=zr-…
export ANTHROPIC_MODEL=standard-class

# OpenAI 风格
export OPENAI_BASE_URL=http://127.0.0.1:8787/v1
export OPENAI_API_KEY=zr-…
```

```sh
curl http://127.0.0.1:8787/v1/messages -H "x-api-key: $TOKEN" \
  -H 'content-type: application/json' -d '{
    "model": "reasoning-class", "max_tokens": 256, "stream": true,
    "messages": [{"role": "user", "content": "hi"}] }'
```

响应头 `x-zroutery-model` / `x-zroutery-provider` 标明实际应答方；`x-zroutery-degraded: 1` 表示所有候选
都在熔断中的兜底调用；已配价格的模型带 `x-zroutery-cost`（如 `CNY 0.000048`）。

`claude-sonnet/opus/haiku` 系模型名默认映射到对应 class，可在 Routing 关闭或用别名覆盖。

## 花费与预算

在 Models 里填价格（每百万 token，按 provider 计费币种），Zroutery 按上游返回的 usage 记账：

- 日志、按模型汇总、总计都带金额，**按币种分开统计**，绝不把 USD 和 CNY 加到一起；
- 缓存命中按缓存读价计，不与输入价重复计费；没填缓存价退回输入价（只高估不低估）；
- 没填价格的模型记为「无价格」而非 0，总计是下限；
- `POST /v1/messages/count_tokens` 额外给出发送前的 prompt 花费估算，写在 `zroutery` 字段里。

预算护栏：给 全部 / provider / class 设日或月上限，超额后**拒绝**（402 + 说明是哪条限额）或
**降级到便宜的 class**。花费落盘（`spend.json`，定时 10 秒 + 退出时）；依然**不换算币种**；
每个有配额的 scope 同时只放行一个请求，保证「最多超出一个请求的量」；降级不能绕开限额，
被预算拦下的请求不重试、不失败转移、不计健康度。

## Classifier Routing（Auto Mode）

Claude Code Auto Mode 的判定侧查询（温度 0、`max_tokens` 64、`stop_sequences` 含 `</block>` 等）
按**请求形态**（打分制指纹）识别，送入独立候选池：

```json
"classifier": {
  "enabled": true, "strategy": "priority", "failover": true, "max_attempts": 2,
  "candidates": [{"model": "zai-glm-5.3", "priority": 10}],
  "detection": {"enabled": true, "minimum_confidence": 0.85}
}
```

- 候选引用已有模型，复用 provider / 密钥 / 协议 / 价格 / 健康度，没有第二套配置；
- 主请求永不进分类器池、分类器请求永不进主池（集成测试锁死）；
- 判定协议保真：`stop_sequences` 跨方言保持数组，provider quirks 对分类器请求不生效；
- **fail closed**：候选全失败或输出无 `<block>` 判定时错误原样还给 Claude Code，不擅自放行；
- 指纹可配置（`detection.signatures`），Claude Code 升级改配置即可。

## ML 路由

`crates/zroutery-core/src/ml/` 里是一套自适应路由学习子系统，让模型选择可以从真实流量中学习：

- **闭环**：traces 落盘（跨重启、带指纹）→ 时间切分训练 → 与四组基线（priority / round_robin /
  lowest_latency / balanced）在同批真实 trace 上重放对比 → shadow 影子预测统计一致率与收益 →
  九条准则的晋升门禁 → 原子指针激活 / 一键回滚；
- **可复现**：切分、holdout、seed、dataset fingerprint、commit 全程确定，同一份流量判决一致；
- **可逆**：`POST /v1/ml/promote?install=true` 才把模型挂到 live router（判定与安装分离），
  `rollback` 随时回退；`/v1/ml/status` 和 `/v1/ml/shadow` 可观测当前状态；
- **受控探索**：确定性探索且限定在 eligible 集合内，有上限；learn → 冷却 → 再晋升可循环；
- **实测**：相对 priority 的 utility +2.00，相对 balanced 持平但一致更便宜；provider 降级时
  已晋升模型 120/120 请求迁移到幸存者。

详见 [docs/development/ml-closed-loop-report.md](docs/development/ml-closed-loop-report.md)。

## 余额查询

provider 上挂一个 probe（路径 + JSON pointer）：DeepSeek `/user/balance`、Moonshot `/users/me/balance`、
SiliconFlow `/user/info`、OpenRouter `/credits`、Sub2API `GET /v1/usage`（路径随 dialect 变）、
或自定义；OpenAI / Anthropic 无此接口，预设为 not supported。添加或编辑 provider 时直接选预设，
手动点 Check 才查（不做定时轮询）。命令行：`zroutery-headless --balances`。

## 端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/v1/messages` | Anthropic Messages，SSE |
| POST | `/v1/messages/count_tokens` | 本地 token + 价格估算，不打上游 |
| POST | `/v1/chat/completions` | OpenAI Chat Completions，SSE |
| POST | `/v1/responses` | OpenAI Responses，SSE；答案存内存供取回（默认上限 1000 条） |
| GET/DELETE | `/v1/responses/{id}` | 取回 / 删除已存响应 |
| POST | `/v1/responses/{id}/cancel` | 取消响应，留 `cancelled` 占位 |
| POST | `/v1/generateContent` | Gemini 原生入口（无 SSE） |
| GET | `/v1/models`、`/v1/models/{id}` | 同时满足 Anthropic / OpenAI 两种客户端 |
| GET | `/v1/status` | 版本、模型数、provider 数（需 token） |
| GET | `/v1/ml/status` 等 | ML 路由的状态 / shadow / promote / rollback |
| GET | `/health` | 唯一免鉴权路由 |

`/v1` 前缀可省；路径写错返回 JSON 列出真实端点（重复 `/v1/v1/` 会提示去掉），方法用错返回 405。
超预算返回 402 `budget_exceeded`。

## 安全

- 默认只监听 `127.0.0.1`，要求 `x-api-key` 或 `Authorization: Bearer <token>`，定长比较防时序泄露；
- 改 `0.0.0.0` 会让同网段可用你的 key，界面红色告警 + 配置校验 warning；
- token 不进前端（界面只显示末四位，Reveal 单独取一次，Copy 在 Rust 侧写剪贴板）；
- API key 只从钥匙串读；`ZROUTERY_KEY_*` 环境变量仅 `zroutery-headless` 认，GUI 不认；
- 请求体上限默认 32 MiB（可调），超限 413；CORS 默认关闭，打开但不填 origin 会告警；
- 请求日志只在内存（环形缓冲，默认 500 条），退出即消失；配置文件不含密钥。

## 协议转换

四个入口方言共用一套转换（每个方言 decoder + encoder），已覆盖：文本、system prompt、多轮、
工具调用（含流式增量 JSON）、工具结果、图片、thinking ↔ `reasoning_content`、停止原因、
usage（含缓存命中与 reasoning tokens）、`stop_sequences` ↔ `stop`、`reasoning_effort` ↔ thinking budget。

已知限制：`n > 1` 只取第一个 choice；`count_tokens` 是估算；Anthropic `signature` 转 OpenAI 方言会丢失；
流式一旦开始就不再转移，中途断只能透传错误；音频 / 文件 / server-side tools 块会被丢弃；
Gemini 只接非流式 `generateContent`；`/v1/responses` 存储为内存有界，非持久化。

## 项目结构

```
crates/zroutery-core/      协议转换、注册表、路由、计费、预算、HTTP 服务（无 GUI 依赖）
  src/ir/                  统一中间表示：每方言一套 decoder + encoder
  src/protocol/            anthropic / openai / responses / gemini + SSE 状态机
  src/billing.rs           价格计算（按币种）、余额 probe
  src/budget.rs            支出账本（落盘）与限额判定
  src/config.rs            provider、模型身份与 id 推导、配置迁移
  src/election.rs          延迟+价格打分（纯函数）
  src/router.rs            类内候选排序、健康度、熔断、失败转移
  src/server/              axum 路由、鉴权、选举、请求管道
  src/ml/                  ML 路由：特征、bandit、shadow、冷却与晋升/回滚
  src/account/             可选账号子系统（feature "account"，含 newapi 适配器）
src-tauri/                 桌面外壳：菜单栏、钥匙串、配置持久化、Tauri 命令
ui/                        React + TypeScript 仪表盘（Overview/Providers/Models/Routing/ML/Activity/Settings）
scripts/                   冒烟测试、布局测试、打包与工具链门禁
docs/development/          开发流程、历史处理与子系统设计记录
```

## 开发提示

- provider 的 “Compatibility” 开关对付「OpenAI 兼容」方言差异：拒收 `max_tokens` / `temperature`、
  不认 `stream_options` 等；
- `cargo test -p zroutery-core` 只跑纯逻辑，秒级；`--all-features` 会编译 `ml` / `account`；
  `pnpm smoke` 验证真实进程；`ZROUTERY_LOG=debug` 看请求细节；
- 价格单位是每百万 token，目录自动填的价格已换算；
- 开发流程与子系统设计记录见 [docs/development/](docs/development/README.md)。
