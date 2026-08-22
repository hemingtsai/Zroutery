# Zroutery

把多个 LLM provider 聚合成**一个**本地端点，同时提供 Anthropic Messages API 和 OpenAI Chat
Completions API。除了真实模型 id，还额外暴露 `opus-class` / `sonnet-class` / `haiku-class`
三个虚拟模型，后端按你**手动指定**的级别去选模型。

macOS 桌面应用：常驻菜单栏，无 Dock 图标，关窗不退出。

```
客户端 ──┬─ POST /v1/messages          (Anthropic 方言)
         └─ POST /v1/chat/completions  (OpenAI 方言)
                    │
              统一 IR + 路由 + 失败转移
                    │
         ┌──────────┴──────────┐
    DeepSeek (OpenAI 兼容)   OpenAI / Anthropic / Ollama / vLLM …
```

## 快速开始

前置：Rust 1.80+、Node 20+、pnpm、Xcode Command Line Tools。

```sh
pnpm install                # 装 tauri CLI 和前端依赖
pnpm dev                    # 开发模式（热更新）
pnpm build                  # 产出 target/release/bundle/{macos,dmg}
pnpm test                   # cargo test --workspace
pnpm smoke                  # 起一个假 provider，端到端跑通两种方言、流式、计费、选举
pnpm test:layout            # 先自检判定逻辑，再用无头 Chromium 量真实界面的控件
```

> `pnpm build` 里的 DMG 步骤用 `hdiutil` + Finder，需要在正常桌面会话里跑；只要 `.app`
> 的话用 `pnpm tauri build --bundles app`。打出来的 `.app` 约 13MB，里面同时带了
> `zroutery-headless`，可以直接从 bundle 里启动无界面代理。

首次运行会在 `~/Library/Application Support/app.zroutery.desktop/config.json`
生成配置和一个本地 token。API key 存 **macOS 钥匙串**，不落配置文件。

无图形环境时可以只跑代理：

```sh
cargo run -p zroutery --bin zroutery-headless
# 可选：ZROUTERY_CONFIG_DIR=/path/to/dir  ZROUTERY_KEY_PROVIDER_DEEPSEEK=sk-xxx
```

## 三步配置

1. **Providers**：加一个 provider（选 OpenAI 兼容或 Anthropic），填 base URL，粘 API key。
   点 “Fetch models” 可以把上游模型列表拉下来。
2. **Models**：给每个模型选一个 class。**级别永远由你指定，程序不猜**。没选级别的模型仍可以用
   精确 id 调用，但不参与 `*-class` 路由，界面会一直提醒你。
3. **Routing**：选类内策略（下面详述）、失败转移次数、熔断阈值。

### 类内怎么选模型

Routing 面板里五种策略：

| 策略 | 行为 |
| --- | --- |
| **Balanced（选举）** | 按实测延迟 + 价格综合排序，见下 |
| Priority | 优先级数字小的先用，同级按权重随机 |
| Weighted random | 按权重随机分摊 |
| Round robin | 轮流，忽略优先级 |
| Lowest latency | 谁历史上快用谁 |

#### 选举（Balanced）

手动排优先级的问题是：同一个 class 里三个模型谁该当默认，取决于它们今天的价格和响应速度，
而这两件事配置文件里看不出来。所以 Balanced 靠一次**选举**：

1. 给每个 class 的每个成员发一个 **1 token** 的最小请求，量往返延迟；
2. 用延迟和价格一起打分，**排好序钉住**；
3. 之后一直用这个顺序，直到下次选举。

**不是每个请求重算**——探测要花一个请求，而且路由如果随负载漂移就没法排查问题了。什么时候跑：

- 启动时跑一次（Routing 里可关，默认开）；
- 界面上点 **Re-run now**；
- 命令行 `zroutery-headless --elect`，打印顺序然后退出。

打分的几个刻意决定：

- **每个轴都按「是类内最好的几倍」算分**（1.0 就是最好），这样倍数关系不会丢：贵 50 倍会压过慢 2 倍，
  而不是两者都糊成「差一点」。单轴最多算到 100 倍封顶——否则「免费 vs 收费」是无穷倍，
  另一个轴就再也说不上话了；免费模型也因此不会除零。
- **价格只有在「每个应答的成员都有价格」且「币种一致」时才参与**。没法诚实地比较 2 CNY 和 3 USD，
  也没法猜一个没标价模型的成本，所以这种情况退化成只看延迟，并且**把原因写在界面上**，
  而不是偷偷编一个数。
- 价格是每百万 token 的，要能比较必须先定一个**参照请求**（默认 1000 in / 500 out，可改）。
- 权重是相对值，填 `3` 和 `1` 等于填 `0.75` 和 `0.25`。
- 探测失败的模型排最后、不给分，错误原因留着。
- 完全平手时退回手填的优先级，再退回 id，保证两次跑结果一致。

选举没见过的模型（之后新加的，或者当时没应答的）排在见过的后面，按优先级。
也就是说新模型立刻可用，但不会悄悄拿到它从没被测过的首选位。

探测本身是走正常编码路径的真实请求，所以它同时也是最新的健康信号，会记进健康表。

### 模型 id 规则

模型的身份是 **(provider, 上游模型名)**，对外 id 由这两者推导：`<provider>-<模型名>`，不单独存储，
所以不会漂移，也不会因为两个 provider 提供同名模型而冲突：

```
deepseek + deepseek-v4-pro    →  deepseek-deepseek-v4-pro
openrouter + deepseek-v4-pro  →  openrouter-deepseek-v4-pro
openrouter + deepseek/r1:free →  openrouter-deepseek-r1-free   （/ 和 : 会变成 -）
```

发给上游的仍然是原始模型名（`deepseek-v4-pro`），只有对外的 id 带前缀。嫌长可以在模型详情里加
**aliases**，任意短名都能解析到同一个模型。

从 0.1.x 升级：旧配置里手写的 `id` 会自动变成 alias，老客户端不用改；界面会提示一次新 id 是什么。

按简报里的例子配完之后，对外可用的模型是：

```
deepseek-deepseek-v4-flash (haiku)   deepseek-deepseek-v4-pro (sonnet)
openai-gpt-5.3-sol (opus)
opus-class          sonnet-class          haiku-class
```

## 接客户端

Anthropic 风格（含 Claude Code）：

```sh
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
export ANTHROPIC_AUTH_TOKEN=zr-…        # 界面里 Copy token
export ANTHROPIC_MODEL=sonnet-class
```

OpenAI 风格：

```sh
export OPENAI_BASE_URL=http://127.0.0.1:8787/v1
export OPENAI_API_KEY=zr-…
```

```sh
curl http://127.0.0.1:8787/v1/messages -H "x-api-key: $TOKEN" \
  -H 'content-type: application/json' -d '{
    "model": "opus-class", "max_tokens": 256, "stream": true,
    "messages": [{"role": "user", "content": "hi"}]
  }'
```

响应头里会带 `x-zroutery-model` / `x-zroutery-provider`，告诉你这次实际是谁答的（`x-zroutery-model`
就是 `<provider>-<模型名>` 那个 id）；`x-zroutery-degraded: 1` 表示所有候选都在熔断中，属于兜底调用；
配了价格的模型还会带 `x-zroutery-cost`，例如 `CNY 0.000048`。

Claude 系命名（`claude-sonnet-4-5-…`、`claude-3-5-haiku-…`）默认按名字里的
opus/sonnet/haiku 映射到对应 class，可以在 Routing 里关掉或用精确别名覆盖。

## 花费估算

在 Models 里给模型填价格（**每百万 token**，币种就填 provider 计费用的那个），Zroutery 会用上游返回的
usage 算出每次请求的花费：

- 请求日志、按模型汇总、总计都带金额，并且**按币种分开**统计，绝不把 USD 和 CNY 加到一起。
- 缓存命中按「缓存读价」计，并且不会再按输入价重复算一遍（各家的 `cached_tokens` 都含在 prompt 总数里）；
  不填缓存价就退回输入价，只会高估不会低估。
- 没填价格的模型记为「无价格」而不是 0，所以总计是下限，不是账单。
- `POST /v1/messages/count_tokens` 除了 `input_tokens`，还在 `zroutery` 字段里给出这次会由谁回答、
  以及**发送之前**的 prompt 花费估算：

```json
{ "input_tokens": 42,
  "zroutery": { "estimated": true, "model": "deepseek-deepseek-v4-pro",
                "estimated_input_cost": { "currency": "CNY", "amount": 0.000084 },
                "input_per_mtok": 2.0, "output_per_mtok": 8.0 } }
```

从 provider 拉模型列表时，如果目录里带价格（OpenRouter 那种按单 token 计价的字符串），会自动换算成
每百万 token 填好，之后仍可修改。流式请求拿不到 `x-zroutery-cost`（响应头早就发出去了），
但 Activity 里照样记账。

## 预算护栏

给「全部 / 某个 provider / 某个 class」设日或月上限，在 Routing 面板里加。到额之后两种处理：
**拒绝**（返回 402 并说明是哪条限额），或者**降级到便宜的 class**。

三件事决定了它的行为，值得先说清楚：

- **一次请求的花费只有跑完才知道**，所以预算是「越线检测」而不是「预授权」：越线那一次会正常完成，
  下一次才被拦。也就是说最多超出一次请求的量。想做到严格不超，就得靠一个各家都不认的 token 估算，
  那不如老实说。
- **花费会落盘**（`spend.json`，在配置目录旁边）。重启就忘的护栏不算护栏——请求日志是刻意只放内存的，
  所以它不适合用来记账。落盘是定时（10 秒）+ 退出时，硬杀最多丢几秒，不会丢整轮。
- **依然不换算币种**。USD 的预算只统计 USD 花费；如果限额的币种你的模型压根不用，那它永远不会触发，
  校验里会直接警告，而不是假装在保护你。

降级不能用来绕开限额：便宜 class 自己的预算照样生效，一圈降级绕回来会变成拒绝而不是死循环。
被预算拦下的请求不重试、不失败转移、也不计入模型健康度——重试就等于把刚拒掉的钱花出去。

```
$ curl ... -d '{"model":"sonnet-class",...}'
{"error":{"type":"budget_exceeded",
          "message":"stopped by a budget: the today limit for everything (5.00 USD) is used up"}}
```

## 余额查询

各家余额接口没有统一标准，所以 provider 上挂一个 *probe*：一个路径加几个 JSON pointer。内置这些预设：

| 预设 | 端点 | 说明 |
| --- | --- | --- |
| DeepSeek | `/user/balance` | 挂在 API 根而不是 `/v1` 下，所以预设里写的是绝对 URL |
| Moonshot | `/users/me/balance` | |
| SiliconFlow | `/user/info` | |
| OpenRouter | `/credits` | 只给 total 和 usage，相减得余额 |
| **Sub2API** | `/v1/usage` | 中转站，见下 |
| Custom | 自己填 | 路径 + JSON pointer |

OpenAI 和 Anthropic 根本没有这种接口，预设里就是「not supported」，不装样子。

**添加 provider 时就能选**：Providers 面板的新增表单里有 Name / API dialect / Base URL / Balance endpoint
四项，选好直接建好，不用建完再回去改。已有的 provider 也能随时在卡片上改。

### Sub2API

[Sub2API](https://github.com/Wei-Shaw/sub2api) 中转站有 `GET /v1/usage`，返回内容取决于 key 的类型，
但三种情况都带 `remaining` 和 `unit`，所以一个 probe 全覆盖：

- **钱包 key**：`remaining` 就是账户余额；
- **限额 key**：额外给 `quota.limit` / `quota.used`，界面显示成「余额 of 总额」；
- **订阅 key**：`remaining` 是日/周/月里最紧的那个窗口还剩多少。

只配了速率限制（没有金额）的 key 会返回「读不到金额」而不是 0——这是实话，不是余额为零。

它同时提供 Anthropic 和 OpenAI 两套接口，两种 dialect 的 base URL 深度不同（OpenAI 兼容的 base 已经
以 `/v1` 结尾，Anthropic 的 base 是根），所以这个预设的路径**跟着 dialect 变**：前者用 `/usage`，
后者用 `/v1/usage`。鉴权头也各按各的方言发（`Authorization: Bearer` 或 `x-api-key`），Sub2API 两个都收。

它是自托管的，所以 base URL 必须你自己填，界面在选了这个预设且 URL 还空着时会提示。

Providers 面板里每个 provider 一行：选预设 → 点 Check，旁边显示余额和检查时间，失败会留下原因。
**不做定时轮询**——查一次要花一个请求，有些厂商还限流，所以只在你点的时候查。

命令行也能查：

```sh
zroutery-headless --balances     # 逐个查、打印、退出，适合塞进 cron
# deepseek: 48.75 CNY remaining

zroutery-headless --elect        # 跑一次选举，打印每个 class 的排序然后退出
# sonnet-class:
#   deepseek-deepseek-v4-pro     primary: 640 ms, 0.0060 CNY per reference request
#   openai-gpt-sonnet            fallback 1: 710 ms, 0.0600 CNY per reference request
```

## 端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/v1/messages` | Anthropic Messages，支持 SSE |
| POST | `/v1/messages/count_tokens` | 本地 token 估算 + 价格估算，不打上游 |
| POST | `/v1/chat/completions` | OpenAI Chat Completions，支持 SSE |
| GET | `/v1/models`、`/v1/models/{id}` | 同一份 JSON 同时满足两种客户端 |
| GET | `/v1/status` | 版本、模型数、provider 数（需要 token） |
| GET | `/health` | 只回 `{"status":"ok"}`，唯一免鉴权的路由 |

花费超预算时返回 **402**（`budget_exceeded`）。

**`/v1` 前缀可有可无**：上面每个路径去掉 `/v1` 也一样能用（`/models`、`/chat/completions`、
`/messages` …），因为各家客户端对「base URL 要不要带 `/v1`」的约定并不一致。所以
`OPENAI_BASE_URL` 填 `http://127.0.0.1:8787` 或 `http://127.0.0.1:8787/v1` 都行。

路径写错时不会给你一个空的 404，而是返回 JSON 列出真实可用的端点；如果是 `/v1/v1/...`
这种重复前缀（把带 `/v1` 的 base URL 配给了会自己拼 `/v1` 的 SDK，Anthropic 系最常见），
会直接告诉你去掉 base URL 里的 `/v1`。方法用错（比如 GET 打 `/v1/chat/completions`）会返回 405
并说明该用哪个方法。

## 安全

- 默认只监听 `127.0.0.1`，并且要求 `x-api-key` 或 `Authorization: Bearer <token>`。
  能访问这个端口的进程就能花你的额度。
- 改成 `0.0.0.0` 会让同网段的人可用你的 key，界面会红色告警；这种情况下别关鉴权。
- token 比较用的是定长比较，避免时序泄露。
- **token 不进前端**：界面拿到的只有 `zr-…后四位`，点 Reveal 才单独取一次，Copy 是在 Rust 侧
  直接写剪贴板。快照里的 `auth_token` 一律为空，回存时为空表示「保持不变」。
- **API key 只从钥匙串读**。`ZROUTERY_KEY_*` 环境变量只有 `zroutery-headless` 认，GUI 不认
  （环境变量同机可见，还会进崩溃报告和 CI 日志）。
- 请求体上限默认 32 MiB（Routing 面板可调），超限直接 413，不会转发给上游。
- CORS 默认关闭；打开后不填 origin 列表会红色告警（校验里也有对应 warning），填了就只允许
  列出的 origin，方法和请求头也收敛到两个 API 真正用到的那些。
- 请求日志只在内存里（环形缓冲，默认 500 条），进程退出即消失；配置文件里不含任何密钥。

## 项目结构

```
crates/zroutery-core/     协议转换、模型注册表、路由、计费、预算、HTTP 服务（无 GUI 依赖，180 个测试）
  src/ir.rs               统一中间表示：2 个 decoder + 2 个 encoder，避免 N×M
  src/protocol/           anthropic.rs / openai.rs，含两个方向的 SSE 状态机
  src/billing.rs          价格计算（按币种分开）、余额 probe 与五个内置预设
  src/budget.rs           支出账本（落盘）与限额判定：拒绝或降级
  src/config.rs           provider、模型身份与 id 推导、路由策略、配置迁移
  src/registry.rs         模型 id 解析：id/别名走一次哈希，class 成员表预先算好
  src/election.rs         按延迟+价格给类内成员打分排序（纯函数，15 个测试）
  src/router.rs           类内候选排序、健康度、熔断、失败转移
  src/server/mod.rs       axum 路由、鉴权、请求体上限、CORS、选举执行
  src/server/pipeline.rs  单次请求的候选轮询、计费记账、SSE 管道
  src/sync.rs             容忍中毒的锁封装（一个线程 panic 不该拖垮整个代理）
src-tauri/                桌面外壳：菜单栏、钥匙串、配置持久化、Tauri 命令
ui/                       React + TypeScript 仪表盘
scripts/smoke_test.py     端到端冒烟测试（假 provider → 真二进制 → 真 HTTP）
scripts/ui_layout_test.py 无头 Chromium 量界面控件的高度和基线
```

## 协议转换支持情况

已覆盖：文本、system prompt、多轮、工具调用（含流式增量 JSON）、工具结果、图片、
extended thinking ↔ `reasoning_content`、停止原因、usage（含缓存命中和 reasoning tokens）、
`stop_sequences` ↔ `stop`、`reasoning_effort` ↔ thinking budget。

已知限制：

- `n > 1` 只取第一个 choice（Anthropic 方言没有对应概念）。
- `count_tokens` 是估算：各家 tokenizer 不同，没有可共用的实现。
- Anthropic 的 `signature` / `redacted_thinking` 在转成 OpenAI 方言时会丢失，反向没问题。
- 流式一旦开始就不再转移：握手失败可以换下一个候选，中途断了只能把错误透传给客户端。
- 音频、文件、server-side tools 这类块会被丢掉而不是报错。

## 开发提示

- provider 的 “Compatibility” 开关用来对付 “OpenAI 兼容” 的方言差异：推理模型拒绝
  `max_tokens` / `temperature`，部分网关不认 `stream_options`。
- `cargo test -p zroutery-core` 只跑纯逻辑，秒级；`pnpm smoke` 验证真实进程。
- 想看请求细节：`ZROUTERY_LOG=debug`。
- 价格是每百万 token，不是每 token；从目录里自动填的价格已经换算过了。
