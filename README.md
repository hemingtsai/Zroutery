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
pnpm smoke                  # 起一个假 provider，端到端跑通两种方言 + 流式
pnpm test:layout            # 无头 Chromium 渲染真实界面，量每个控件的高度和基线
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
3. **Routing**：选类内策略（优先级 / 加权随机 / 轮询 / 最低延迟）、失败转移次数、熔断阈值。

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
就是 `<provider>-<模型名>` 那个 id）；`x-zroutery-degraded: 1` 表示所有候选都在熔断中，属于兜底调用。

Claude 系命名（`claude-sonnet-4-5-…`、`claude-3-5-haiku-…`）默认按名字里的
opus/sonnet/haiku 映射到对应 class，可以在 Routing 里关掉或用精确别名覆盖。

## 端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/v1/messages` | Anthropic Messages，支持 SSE |
| POST | `/v1/messages/count_tokens` | 本地估算，不打上游 |
| POST | `/v1/chat/completions` | OpenAI Chat Completions，支持 SSE |
| GET | `/v1/models`、`/v1/models/{id}` | 同一份 JSON 同时满足两种客户端 |
| GET | `/health` | 免鉴权，给 GUI 轮询 |

## 安全

- 默认只监听 `127.0.0.1`，并且要求 `x-api-key` 或 `Authorization: Bearer <token>`。
  能访问这个端口的进程就能花你的额度。
- 改成 `0.0.0.0` 会让同网段的人可用你的 key，界面会红色告警；这种情况下别关鉴权。
- token 比较用的是定长比较，避免时序泄露。
- 请求日志只在内存里（环形缓冲，默认 500 条），进程退出即消失；配置文件里不含任何密钥。

## 项目结构

```
crates/zroutery-core/     协议转换、模型注册表、路由、HTTP 服务（无 GUI 依赖，102 个测试）
  src/ir.rs               统一中间表示：2 个 decoder + 2 个 encoder，避免 N×M
  src/protocol/           anthropic.rs / openai.rs，含两个方向的 SSE 状态机
  src/config.rs           provider、模型身份与 id 推导、路由策略、配置迁移
  src/registry.rs         模型 id 解析（精确 id / 别名 / *-class / Claude 命名）
  src/router.rs           类内候选排序、健康度、熔断、失败转移
  src/server.rs           axum 路由、鉴权、流式管道
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
