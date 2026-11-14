# Zroutery History Treatment Map — b1a38c9..38f592c (73 commits)

STATUS: APPROVED (H2, 2026-09-11)

Boundary: 3e52e0139e4f5105108b93f6fc171fafdfbed31c (exclusive) .. 38f592cf24a7e5898b81ecc65b67dce34f868760 (inclusive)
All 73 commits: InfinityNeko <infinityneko@users.noreply.github.com>

## Part A — 逐 commit 处置表（按原始顺序）

| # | OLD SHA | AUTHOR DATE | OLD SUBJECT | ACTION | TARGET | REASON |
|---|---------|-------------|-------------|--------|--------|--------|
| 1 | b1a38c9 | 08-30 13:22 | refactor(router): extract shared candidate planner | KEEP | T01 | 从 plan_class 提取 plan_candidates 共享规划器，池化轮询游标泛化，独立架构节点 |
| 2 | a7970a2 | 08-30 14:07 | feat(core): classifier request model, detector and configuration | KEEP | T02 | 新增 classifier.rs/query.rs/ClassifierConfig（997 行），RequestKind 分类 + 指纹副查询检测 + fail-closed 判定，分类器能力锚点 |
| 3 | 8ff8c2b | 08-30 14:10 | feat(router): classifier candidate pool on the shared planner | SQUASH | T02 | plan_classifier() 经共享规划器生成尝试列表，同能力实质内容 |
| 4 | cdfdc16 | 08-30 14:21 | feat(server): route classifier side queries through their own pool | SQUASH | T02 | 服务端请求分类 + 副查询专用池路由 + 按类别统计，同能力实质集成 |
| 5 | b523aec | 08-30 15:23 | feat(protocol): classifier fidelity mode, [1m] resolution, verdict validation | SQUASH | T02 | EncodeMode::Classifier、[1m] 去前缀解析重试、fail-closed 判定校验，分类器协议保真 |
| 6 | 46bf01b | 08-30 15:35 | feat(ui): Auto Mode classifier configuration and activity | SQUASH | T02 | Auto Mode 配置卡 + Activity 按类别行（296 行 UI），同能力用户可见面 |
| 7 | 2a18749 | 08-30 16:20 | feat(windows): run the desktop app natively on Windows | KEEP | T03 | 平台专用 keyring、platform.rs、Windows 打包/图标 + 凭据管理器测试，独立平台支持。【重排至 8ed74b9 之后，见裁决 #8】 |
| 8 | fab29e5 | 08-30 16:39 | test(integration): Auto Mode classifier end to end | FIXUP | T02 | 432 行 e2e 十场景验证已完成能力，测试收尾。【重排至 2a18749 之前】 |
| 9 | 8ed74b9 | 08-30 16:40 | docs(readme): classifier routing — what it is and its safety boundaries | FIXUP | T02 | README 39 行文档收尾。【重排至 2a18749 之前】 |
| 10 | 401a033 | 08-30 17:09 | feat(import): read CC Switch providers | KEEP | T04 | 新增 ccswitch.rs（565 行）只读解析 SQLite/JSON 双存储 + 预览/导入命令，独立导入能力 |
| 11 | b3a20dd | 08-30 17:42 | feat(import): CC Switch import UI, and open the dashboard on launch | SQUASH | T04 | Providers 页导入卡（210 行）+ 启动开仪表盘，同能力 UI 半边 |
| 12 | e0e734e | 09-04 19:19 | feat(core): media module — image collection, vision fallback, replacement | KEEP | T05 | 新增 media 模块（collect/vision/transform ~450 行）+ VisionConfig；同 commit 捆绑 panels→pages UI 重构（~3400 行）与窗口生命周期。【见裁决 #1】 |
| 13 | 07c3656 | 09-04 20:31 | feat(core): vision fallback in the pipeline — preflight and reactive | SQUASH | T05 | 管道接入 apply_vision_fallback（预检 + 反应式，+157 行 + 322 行测试矩阵），同能力实质 |
| 14 | 3119eff | 09-04 21:11 | feat(ui): vision fallback settings | SQUASH | T05 | 视觉回退设置面（84 行），同能力设置面 |
| 15 | 9d130fd | 09-04 21:39 | fix(core): stop leaking upstream error bodies to clients | KEEP | T06 | 关闭三条上游错误体泄漏路径（to_wire/activity/含账号标识 URL）+ MiMo 404 回归测试，独立安全修复 |
| 16 | cab9348 | 09-04 22:00 | fix(core): redact model-health errors; add Bearer auth for Anthropic relays | KEEP | T07 | bearer_auth 兼容契约（中继并行 Authorization + x-api-key）修复 scnet 400 + safe_message 统一，独立兼容修复 |
| 17 | f81f6de | 09-05 08:26 | fix(core): Gemini protocol correctness + pipeline vision fallback parity | KEEP | T08 | IR 新增 ToolResult.name 四方言编解码 + Gemini functionResponse 修复 + BlockStop 流生命周期，独立协议修复（+92 行管道视觉流式对齐随行） |
| 18 | 27d233a | 09-05 08:27 | fix(core): correctness fixes in billing, circuit breaker, classifier, SSE, upstream | KEEP | T09 | 六子系统独立微修（billing 精度/断路器锁内竞态/分类器容差/SSE 跨块 CRLF/连接超时可配），无共同能力锚点 |
| 19 | aac2cfa | 09-05 08:27 | fix(tauri,ui): error handling, accessibility, i18n, async safety | KEEP | T10 | 多表面加固（spawn_blocking 导入/JoinError/密钥环境名冲突/aria/i18n/卸载后 setState），无法归入单一能力 |
| 20 | 2cf46e8 | 09-05 08:28 | fix(tauri): minor hardening — keychain matching, key length, config dir fallback | SQUASH | T10 | 11 行同会话 tauri 加固，并入相邻加固节点 |
| 21 | e00195e | 09-05 08:32 | fix(core,tauri): dialect-aware auth, responses index, tool warnings, shutdown | KEEP | T11 | 按方言应答认证错误 + Responses output_index 跨界跟踪 + watch channel 优雅关账，独立用户可见修复 |
| 22 | af74cce | 09-05 08:43 | chore: clippy fixes — zero warnings | FIXUP | T11 | 实质为协议解码器饱和转换加固 + parse_arguments 往返修复 + 4 项 clippy，评审系列收尾 |
| 23 | ea75cd2 | 09-05 08:44 | fix(ui): validate ModelClass cast in budget scope parser | FIXUP | T11 | 预算解析器一行 CLASSES.includes 校验，按序并入扫尾节点（避免跨节点回移冲突） |
| 24 | 2ae4ad3 | 09-05 08:49 | fix(core): remaining low-priority fixes — saturating casts, safe_message, round-trip, docs | FIXUP | T11 | 同评审延续（Responses 生命周期事件/safe_message/数据 URL 校验/配置告警），当日被 T15 编码器系列吸收 |
| 25 | f222ef6 | 09-05 08:56 | test(core): fill all identified coverage gaps — 16 new tests | FIXUP | T11 | 纯测试提交（453 行 8 个既有能力面补覆盖），完成性提交 |
| 26 | 7248039 | 09-05 10:08 | feat(ui): toasts, confirm dialogs, busy indicator, field-level errors | KEEP | T12 | 全局 toast/ConfirmDialog/busybar/字段级校验，468 行 9 文件新 UI 运行时层 |
| 27 | ea7ee17 | 09-05 10:20 | refactor(ui): tone down the new feedback chrome to match the design system | FIXUP | T12 | 12 分钟后对同一 chrome 的设计系统化 + 本地化，设计迭代收尾 |
| 28 | 2794099 | 09-05 11:39 | refactor(core): ModelClass → ModelTier — 4-tier model abstraction | KEEP | T13 | 四档 ModelTier 枚举 + ModelCapabilities 七标志 + BudgetScope/TierElection 改名（25 文件 serde 兼容），后续所有阶段的基础词汇 |
| 29 | c864a6c | 09-05 12:30 | refactor(core): Stage 1 completion — capabilities, naming style, cleanup, contract tests | KEEP | T14 | NamingStyle 契约（12 组合显示名/虚拟 ID 矩阵）+ 规范 ModelCapabilities + tier 契约测试，"completion"标题下的实质 Stage 1 锚点 |
| 30 | 932adea | 09-05 12:35 | fix(core): /v1/models only exposes virtual IDs for configured naming style | FIXUP | T14 | 3 行注册表接线（命名样式接入 /v1/models），纯 wiring |
| 31 | 826e47f | 09-05 13:06 | fix(core): Stage 1 final — Fable tests, naming style contract, class comment cleanup | FIXUP | T14 | 3 个契约测试 + 注释清理 + capabilities UI 接线，收尾 |
| 32 | cf0e533 | 09-05 13:10 | chore: add .zcode/ to .gitignore | FIXUP | T14 | 3 行 .gitignore，微 chore |
| 33 | 7dfd6dc | 09-05 13:30 | fix(core): supports_* fields are now read-only migration layer | FIXUP | T14 | supports_* 只读迁移层（skip_serializing + normalize），Stage 1 tier 契约弃用加固 |
| 34 | a330d14 | 09-05 14:02 | feat(core): Stage 2 — Canonical IR extension, content policy, capability filtering | KEEP | T15 | Stage 2 契约集锚点：IR ContentBlock 新变体（File/Audio/Video/Citation）+ UnsupportedContentPolicy + required_capabilities 路由过滤 |
| 35 | 26a5bfb | 09-05 14:21 | fix(core): Stage 2 P0+P1 — content policy, Gemini MIME, typed capabilities | FIXUP | T15 | 修正新鲜代码（Transform 错误语义/Gemini MIME 分派）+ 能力 Vec→typed enum 细化 |
| 36 | 2aa1b05 | 09-05 14:37 | refactor(core): ModelCapabilities.supports() + router simplification | FIXUP | T15 | 纯重构：能力匹配移入 supports() + 路由简化 |
| 37 | 02b6029 | 09-05 14:46 | feat(ir): Response Store — lifecycle infrastructure for Responses API | SQUASH | T15 | 有界 ResponseStore（驱逐 + watch 取消，272 行），实质组件（独立节点方案因 ir/mod.rs 依赖序不可拆，见 Part C 风险） |
| 38 | 63cca92 | 09-05 15:25 | feat(core): Responses API lifecycle — store, GET/DELETE/cancel, decode improvements | SQUASH | T15 | /v1/responses/{id} GET/DELETE/cancel 端点 + 管道自动存储 + SSE 取消检测，同能力实质 |
| 39 | ec320e4 | 09-05 15:52 | feat(core): Stage 2 completion — Document/File, capability strict, golden tests | SQUASH | T15 | classify_media Document/File 契约 + CapabilityState 三态 + strict_capability_filter + 金样测试，实质深化 |
| 40 | c297ed3 | 09-05 16:07 | fix(core): Stage 2 P0+P1 — file decode, Gemini URL policy, cancel consistency | FIXUP | T15 | 新鲜代码 P0/P1（file 解码静默丢弃/URL 策略/GET-after-cancel 竞态） |
| 41 | 9afcf0a | 09-05 16:21 | fix(core): Stage 2 truly final — input_file, file refs, Image URL policy, instant cancel | FIXUP | T15 | 最终解码修复（input_file/Reference/Image URL/即时取消） |
| 42 | 7cf5ba7 | 09-05 16:48 | fix(core): Responses transparent pass-through + stream output accumulation | FIXUP | T15 | 文件/图片引用透传 + response.completed 累积，保真修复 |
| 43 | 2f5306a | 09-05 17:05 | fix(core): ResponsesStreamEncoder — correct lifecycle, parallel tools, accumulation | FIXUP | T15 | 编码器状态机重写（output_index/事件序/并行工具累积器/终态去重） |
| 44 | aa84781 | 09-05 17:19 | fix(core): ResponsesStreamEncoder — terminal events, ordering, argument types | FIXUP | T15 | 错误终态去重 + output[] 排序 + arguments JSON 字符串修正 |
| 45 | 16b76a0 | 09-05 17:40 | fix(core): ResponsesStreamEncoder — parallel tool lifecycle, terminal events | FIXUP | T15 | per-index ToolOutputState 重构支持并行工具生命周期 |
| 46 | aab0158 | 09-05 17:53 | fix(core): incomplete tool terminal events + 6 streaming regression tests | FIXUP | T15 | 未完成工具终态事件 + 流式回归测试，Stage 2 冻结点（stage-2 tag 锚） |
| 47 | 9314776 | 09-05 19:06 | feat(core): Stage 3A — Policy Foundation, TaskProfile, Eligibility, Scoring, Client Profiles | KEEP | T16 | 独立策略地基：policy.rs 1466 行 + TaskProfile + plan_with_policy + 一致性测试；附带 1696 行协议金样套件（见裁决 #6） |
| 48 | acc3c2f | 09-05 20:06 | feat(core): Stage 3B — Policy Runtime Integration | KEEP | T17 | PolicyFallback 真实执行（reject/escalate/degrade/ignore）+ CapabilityState 资格语义 + 请求感知成本打分 + handle_chat 接线 |
| 49 | cac085c | 09-05 20:46 | fix(core): Stage 3B wiring — TaskProfile scoring, streaming, fallback context | FIXUP | T17 | 3B 接线修复（TaskProfile 硬编码 None/流式 matcher false/回退档位陈旧） |
| 50 | 182a17d | 09-05 22:45 | feat(core): Stage 3C — Decision Trace, Policy Diagnostics, Replay | KEEP | T18 | 决策追踪能力：RouteDecision/CandidateDecision/DecisionReason + 逐候选资格得分 + 回退链 + StoredResponse/活动日志存储 |
| 51 | 449c2dc | 09-05 23:14 | feat(core): Stage 3C.1 hardening + Stage 4A Runtime Observation Foundation | KEEP | T19 | 84% 为独立观测地基（observation.rs 568 行）+ 16% 3C.1 策略修订加固（见裁决 #2） |
| 52 | e2c047d | 09-05 23:28 | test(core): 4A Hardening — temporal, isolation, state machine, invariants, score semantics | FIXUP | T19 | observation.rs 内 26 个新单测（新鲜度边界/健康状态机/得分不变量） |
| 53 | 9e393c9 | 09-05 23:40 | fix(core): ObservationStore keys on (provider_id, model_id) — proper isolation | FIXUP | T19 | ObservationStore 键 model_id→(provider_id, model_id) 隔离修复 + get_best() |
| 54 | b8923c6 | 09-06 06:49 | feat(core): Stage 4B — Observation-Aware Adaptive Scoring | KEEP | T20 | 反馈闭环：Router score_and_sort 读 ObservationStore（含遗留回退）+ 管道全路径记录结果 |
| 55 | 5f60fb3 | 09-06 07:14 | feat(core): Stage 4B Hardening + 4C Failure Semantics | KEEP | T21 | 76% 为全新 failure.rs 契约（10 变体 FailureClass + FailureImpact 不变量），被 T22/T24/T25 消费的 load-bearing 契约（见裁决 #3） |
| 56 | 0879e5e | 09-06 07:39 | feat(core): Stage 4D — Runtime Statistics (EWMA, P50, P95, Failure by Class) | KEEP | T22 | 独立 stats_ext.rs（EWMA/流式百分位环/LatencyStats/按类 FailureStats/StatsStore），与打分不同的统计层 |
| 57 | c0f059c | 09-06 09:05 | fix(core): Stage 4D completion — percentile ring, alpha validation, runtime wiring | FIXUP | T22 | 环形缓冲驱逐 bug 修复 + EWMA alpha 校验 + StatsStore 接线 |
| 58 | cc4763b | 09-06 10:05 | feat(core): Stage 5 — Optional Account Component | KEEP | T23 | 特性门控账号子系统 4 新文件（身份/状态/能力 + 配额/用量/限流 + 线程安全存储 + AccountProvider trait） |
| 59 | 47fdad0 | 09-06 10:06 | feat(core): Stage 6 — Outcome & Feedback Model | KEEP | T24 | 契约族：outcome.rs（Outcome/Attempt/FinalStatus + 决策关联）+ feedback.rs（FeedbackSignal/DataOrigin/训练样本边界）；原消息的管道接线声明与 diff 不符，已在新消息中删除 |
| 60 | 29f8f0a | 09-06 10:48 | feat(core): Stage 7A — Feature Extraction (RoutingFeatures + FeatureExtractor) | KEEP | T25 | ml/features.rs：固定 [f32;32] schema v1 + FeatureExtractor 覆盖五组特征 |
| 61 | 8a0cfe6 | 09-06 10:50 | fix(core): MiMo model discovery compatibility + normalize model list parsing | KEEP | T26 | extract_model_list() MiMo 风格解析 + id 回退 + 裸数组 + 回归测试；捎带 80 行 7A 基准文件（见裁决 #4） |
| 62 | b0efa24 | 09-06 10:51 | feat(tauri): Integration I1 — CC Switch import validation and reporting | KEEP | T27 | ImportReport/预写校验/仅钥匙串 API key 处理（ccswitch.rs +388），区别于 T04 导入能力 |
| 63 | b855707 | 09-06 10:59 | feat(core): Session routing — Free/Sticky/Pinned modes | KEEP | T28 | 新 session.rs（343 行）：三模式 + 自动晋升 + 线程安全存储 + 迁移不变量测试 |
| 64 | 7667405 | 09-06 11:01 | feat(core): Stage 7B — Training Dataset (TrainingSample + Targets + DatasetStore) | KEEP | T29 | 新 ml/dataset.rs（709 行）：TrainingSample/Targets/SampleBuilder + 有界存储年龄驱逐 + 校验 |
| 65 | 8bcafbb | 09-06 11:22 | feat(core): Stage 7C — RoutingModel abstraction + 4 specialized models | KEEP | T30 | 新 ml/model.rs（1095 行）：RoutingModel trait + ModelState 校验和 + 4 个在线模型 |
| 66 | 7ffec9c | 09-06 11:54 | feat(core): 7C Hardening + Integration I2/I3/I4 | SQUASH | T30 | 模型加固 731 行 + migration.rs/agent_takeover.rs 模块诞生（660+624 行）——按框架归 T30，文件级拆分需 edit 手术（见裁决 #5） |
| 67 | eaf7693 | 09-06 12:09 | fix(core): 7C Quantitative Closure — ML gate, dimension validation, benchmarks | FIXUP | T30 | load() 维度 + NaN/Inf 校验、ml feature 门、基准收尾 |
| 68 | 8361d4e | 09-06 12:30 | feat(core): I2 Executor + I3 Agent Adapters — 46 new integration tests | KEEP | T31 | MigrationExecutor 步骤逻辑 + 三代理适配器 + 集成测试 + samples_from_outcome 尝试级归因；其 evaluation 模块声明在 T32 才有实现（原始历史即如此，见裁决 #7） |
| 69 | 1d2842f | 09-06 12:39 | feat(core): Stage 7D — Evaluation framework + attempt-level attribution | KEEP | T32 | ml/evaluation.rs（841 行）：PredictionMetrics/RoutingMetrics/ComparisonReport/Evaluator + 集成测试 |
| 70 | d92b19b | 09-06 13:06 | feat(core): Reward/Utility + I2 Real Execution + NewAPI Adapter | KEEP | T33 | ml/reward.rs（399 行）+ FrozenHoldout/temporal_split + I2 真实执行 532 行 + NewAPI 适配器 159 行 |
| 71 | 707e9a3 | 09-06 13:37 | feat(core): 4-line loop closure — Coordinator + NewAPI Auth + I2/I3 Real + I4 Restore | SQUASH | T33 | Coordinator 决策层（300 行：资格>会话>效用 + 滞回）+ I2/I3/I4 真实执行 + NewAPI 认证，闭环完成 |
| 72 | f0ea770 | 09-06 15:17 | refactor(core): clippy fixes — cosmetic lints on frozen contract files | FIXUP | T33 | 行为保持的 clippy 扫尾（12 文件，含本节点 agent_takeover.rs/ml/dataset.rs）；不回移冻结契约文件以保全阶段 tag 树 |
| 73 | 38f592c | 09-06 15:17 | feat(core): Stage 7E-0 — Immutable Model Identity + Replay Foundation | KEEP | T34 | 新 ml/model_identity.rs（1315 行）：ModelId/CommitId/血统链 + ModelEnsemble/ModelStore + 确定性 ReplayEngine |

## Part B — 目标节点表（34 节点，按新历史顺序）

| TARGET | NEW SUBJECT | NEW BODY | SOURCE (anchor 加粗) | STAGE TAG | 备注 |
|---|---|---|---|---|---|
| T01 | refactor(router): extract shared candidate planner | Generalize pool planning out of plan_class with pool-keyed round-robin cursors so any candidate pool can reuse it. | **b1a38c9** | | 框架 T01 |
| T02 | feat(router): add request classifier with dedicated pool | Classify inbound requests by kind with fingerprint-based side-query detection and route them through a dedicated candidate pool built on the shared planner. Enforce protocol fidelity for classified traffic (classifier encode mode, [1m] resolution retry, fail-closed verdict validation) and add the Auto Mode settings card, per-kind activity stats, e2e coverage and README documentation. | **a7970a2** +8ff8c2b +cdfdc16 +b523aec +46bf01b +fab29e5 +8ed74b9 | | fab29e5/8ed74b9 重排至 2a18749 之前 |
| T03 | feat(tauri): run the desktop app natively on Windows | Add target-specific keyring backends, a platform module for config directory and shutdown signaling, Windows bundles and icons, and a Credential Manager round-trip test. | **2a18749** | | 从位置 7 重排至 8ed74b9 之后 |
| T04 | feat(tauri): import providers from CC Switch | Read both SQLite and JSON CC Switch stores read-only with tested mapping rules, expose preview/import commands with credential-store key handling, and add the providers-tab import card. Open the dashboard on launch. | **401a033** +b3a20dd | | 与 T27（校验）为两个节点 |
| T05 | feat(core): add media pipeline with vision fallback | Introduce the media module (collect/vision/transform) with VisionConfig, wire preflight and reactive fallback into the request pipeline, and add the settings surface. As originally authored, this commit also carries the panels-to-pages UI restructure with i18n and desktop window lifecycle. | **e0e734e** +07c3656 +3119eff | | 裁决 #1：捆绑 UI 重构的披露 |
| T06 | fix(core): stop leaking upstream error bodies to clients | Close three leak paths (wire responses, activity records, transport URLs carrying account identifiers) and add a regression test on the field-reported MiMo 404 body. | **9d130fd** | | |
| T07 | fix(core): redact health errors and add relay bearer auth | Unify error redaction through safe_message for model-health errors, and send Authorization alongside x-api-key for Anthropic-compatible relays via the new bearer_auth provider option, fixing relay 400s. | **cab9348** | | |
| T08 | fix(protocol): correct Gemini tool results and stream lifecycle | Add ToolResult.name to the IR and encode/decode it across all four dialects, fix the Gemini functionResponse tool-use path, and correct BlockStop stream lifecycle with tool-delta buffering. | **f81f6de** | | |
| T09 | fix(core): correctness fixes across billing, breaker and SSE | Fix billing f64 rounding, a circuit-breaker race under lock, classifier tolerance, SSE CRLF across chunk boundaries, and make the upstream connect timeout configurable. | **27d233a** | | |
| T10 | fix(tauri): harden import, secrets and accessibility | Move import work to spawn_blocking, propagate JoinErrors, avoid secrets env-name collisions, add aria-labels, close i18n gaps, guard setState after unmount, and bound Windows Credential Manager keys with case-insensitive keychain matching. | **aac2cfa** +2cf46e8 | | |
| T11 | fix(core): dialect-aware auth errors and review fixes | Answer auth errors in the client's dialect, track Responses output_index across text/thinking/tool boundaries, and shut down the ledger gracefully via watch channel. Folds in the same-day review sweep: saturating protocol-decoder casts, safe_message sanitization, data-URL validation, coverage tests and clippy. | **e00195e** +af74cce +ea75cd2 +2ae4ad3 +f222ef6 | | 4 个评审收尾 commit 按原序并入 |
| T12 | feat(tauri): add toasts, confirm dialogs and field validation | Introduce a global toast system, confirmation dialogs on irreversible actions, a busy indicator, and inline field-level validation, localized and toned to match the design system. | **7248039** +ea7ee17 | | |
| T13 | refactor(core): replace ModelClass with ModelTier | Introduce the four-tier ModelTier enum (Fast/Standard/Reasoning/Frontier) with escalation, a new ModelCapabilities struct, and BudgetScope/TierElection renames with serde backward-compat aliases across 25 files. | **2794099** | | |
| T14 | feat(core): add naming style and capability contract | Add the NamingStyle contract with the full display-name/virtual-ID matrix, canonical capabilities in model metadata, and tier contract tests. Mark deprecated supports_* fields read-only behind a migration layer and wire naming style into /v1/models. | **c864a6c** +932adea +826e47f +cf0e533 +7dfd6dc | stage-1-v1 | 框架 T02，消息按 diff 实质改写 |
| T15 | feat(protocol): add canonical IR and Responses lifecycle | Extend the IR with File/Audio/Video/Citation blocks, content policy and required-capability filtering with a strict mode, plus Document/File classification. Add the bounded ResponseStore with GET/DELETE/cancel endpoints for the Responses API, golden conformance tests, and a correct Responses stream encoder (parallel tools, terminal events, output ordering). | **a330d14** +26a5bfb +2aa1b05 +02b6029 +63cca92 +ec320e4 +c297ed3 +9afcf0a +7cf5ba7 +2f5306a +aa84781 +16b76a0 +aab0158 | stage-2-v1 | 框架 T03；12-commit 连续段，新树与原 aab0158 树一致 |
| T16 | feat(policy): add routing policy foundation | Introduce RoutingPolicy with matchers, requirements, preference scoring and fallback config, TaskProfile, eligibility checks and client profiles, plus the protocol golden conformance suite. | **9314776** | | 框架 T04；裁决 #6 |
| T17 | feat(policy): integrate policy runtime | Execute PolicyFallback actions (reject/escalate/degrade/ignore), add CapabilityState eligibility semantics and request-aware cost scoring, and wire plan_with_policy into request handling. | **acc3c2f** +cac085c | | 框架 T05 |
| T18 | feat(runtime): add decision trace and diagnostics | Record per-candidate eligibility and scores with decision reasons and fallback chains as RouteDecision traces, stored on responses and in the activity log for replay. | **182a17d** | stage-3-v1 | 框架 T06 |
| T19 | feat(runtime): add runtime observations | Add Signal with provenance, freshness and health states, latency/health/cost observations, and a thread-safe ObservationStore keyed on provider and model. Carries policy-revision provenance hardening from the decision-trace work. | **449c2dc** +e2c047d +9e393c9 | | 框架 T07；裁决 #2 |
| T20 | feat(runtime): add observation-aware adaptive scoring | Make router scoring read the ObservationStore with legacy fallback and record outcomes at every success/failure point in the pipeline (buffered, streaming, SSE finalize). | **b8923c6** | | 框架 T08 |
| T21 | feat(core): add failure classification contract | Introduce the ten-variant FailureClass taxonomy with FailureImpact invariants (observation/circuit/retry/fallback/provider-fault) and HTTP-status and error-string classifiers. | **5f60fb3** | | 新节点（原框架并入 T09）；裁决 #3 |
| T22 | feat(runtime): add runtime statistics | Add EWMA, a streaming percentile estimator with ring buffer, latency stats and failure-by-class stats keyed on provider and model, wired into router outcome recording. | **0879e5e** +c0f059c | stage-4-v1 | 框架 T09（标题去掉 failure semantics，归 T21） |
| T23 | feat(account): add optional account component | Add a feature-gated account subsystem: identity, status and capabilities, quota/usage/rate-limit state, a thread-safe store, and the AccountProvider trait. Default builds are unaffected. | **cc4763b** | stage-5-v1 | 框架 T10 |
| T24 | feat(ml): add outcome and feedback model | Define Outcome/Attempt/FinalStatus with decision correlation and dual-store recording, plus FeedbackSignal sources, data origins and the training-sample boundary for model learning. | **47fdad0** | stage-6-v1 | 框架 T11；删除原消息不实的管道接线声明 |
| T25 | feat(ml): add routing feature extraction | Add the fixed 32-dimension RoutingFeatures schema and a FeatureExtractor covering task, candidate, observation, statistics and account feature groups with graceful missing-data handling. | **29f8f0a** | stage-7a-v1 | 框架 T12 |
| T26 | fix(provider): normalize model list discovery | Parse model lists tolerating MiMo-style payloads (models key, model/model_id/name id fields, bare arrays) with diagnostics and regression tests, plus a feature-extraction benchmark. | **8a0cfe6** | | 框架 T13；裁决 #4 |
| T27 | feat(tauri): validate CC Switch imports | Add import reports with draft and batch validation before writing, and keep API keys in the credential store only. | **b0efa24** | | 框架 T15（编号按时间序调整） |
| T28 | feat(runtime): add session routing modes | Add Free/Sticky/Pinned session routing with automatic promotion, thread-safe session storage with eviction, and migration invariants. | **b855707** | | 框架 T14（编号按时间序调整） |
| T29 | feat(ml): add training dataset | Add TrainingSample/Targets with a sample builder from outcomes and a bounded DatasetStore with age eviction and validation. | **7667405** | stage-7b-v1 | 框架 T16 |
| T30 | feat(ml): add routing decision models | Introduce the RoutingModel trait with checksummed state and four online models: AdaGrad logistic success plus linear latency, TTFT and cost regressions, with batch training, dimension and NaN validation behind the ml feature gate and benchmarks. Also seeds the migration and agent-takeover modules executed in the next commit. | **8bcafbb** +7ffec9c +eaf7693 | stage-7c-v1 | 框架 T17；裁决 #5 |
| T31 | feat(integration): add migration and agent takeover execution | Implement the MigrationExecutor step logic with rollback, Claude/Codex/Gemini agent adapters, and attempt-level sample attribution, with integration coverage. | **8361d4e** | | 框架 T18；裁决 #7（中间树沿袭原始损坏） |
| T32 | feat(ml): add evaluation framework | Add prediction and routing metrics, an evaluator, frozen holdout and temporal split, and A/B comparison reports with recommendations. | **1d2842f** | stage-7d-v1 | 框架 T19；归因已在 T31，标题去掉 attribution |
| T33 | feat(ml): close the online routing decision loop | Add reward computation with policy weights, the ActionGuard session/explore decision, and the Coordinator layer (eligibility, session constraints, hysteresis, utility comparison). Wire real migration execution, agent takeover adopt/release/restore, and the NewAPI adapter with quota auth. | **d92b19b** +707e9a3 +f0ea770 | | 框架 T20；裁决 #9 |
| T34 | feat(ml): add immutable model identity and replay | Introduce content-addressed model commits with parent lineage, an ensemble checkpoint of all four models, an in-memory commit store with tags, and a deterministic replay engine. | **38f592c** | stage-7e0-v1 | 框架 T21 |

## Part C — 统计与风险

```
KEEP:   34
SQUASH: 13
FIXUP:  26
Total:  73
预期新节点数: 34
多作者合并节点: 无（73 个 commit 均为 InfinityNeko <infinityneko@users.noreply.github.com>）
需人工裁决项: 9 项（见下）
```

### 需人工裁决项

1. **e0e734e 三合一巨型 commit**（media 模块 + panels→pages UI 重构 + 窗口生命周期，6863 行插入）。默认：整体保留为 T05，body 披露捆绑内容。备选：edit 手术按目录拆为 ui-redesign / media-vision 两节点（超出 pick/fixup 动作词表，需新增 SPLIT 动作）。
2. **449c2dc 双内容**（84% 观测地基 + 16% 3C.1 策略修订加固）。默认：整体 KEEP 为 T19，body 提及策略加固随行。备选：hunk 级拆分策略部分入 T18。
3. **5f60fb3 failure.rs 契约**。框架原将 failure semantics 并入统计节点。默认：独立为 T21（被 T22/T24/T25 消费的 load-bearing 契约）。备选：SQUASH 入 T20 并改消息为 "adaptive scoring and failure classification"。
4. **8a0cfe6 捎带 80 行 7A 基准测试文件**。默认：保留在 T26，body 提及。备选：文件级拆入 T25。
5. **7ffec9c 双内容**（模型加固 731 行 + migration/agent_takeover 模块诞生）。默认：按框架 SQUASH 入 T30，body 说明模块播种。备选：文件级拆分（模型加固→T30，模块诞生→T31 锚点）。约束：eaf7693 与 7ffec9c 均改 model.rs，纯重排不可行。
6. **9314776 附带 1696 行协议金样套件**（逻辑上守护 T15 契约，但策略内容占 62%）。默认：保留在 T16，body 提及。备选：hunk 拆入 T15。
7. **8361d4e 中间树损坏**（ml/mod.rs 声明 evaluation 模块但 T32 才实现——原始历史即如此）。默认：原样保留（不治疗代码）。备选：hunk 手术把声明行移至 T32（会改变 T31 中间树）。
8. **2a18749 重排**（从 46bf01b 与 fab29e5 之间移至 8ed74b9 之后，仅 2 位）。默认：执行最小重排。若 fab29e5/8ed74b9 应用冲突：回退为移至 b3a20dd 之后；再冲突则 ABORT 报告。
9. **d92b19b/707e9a3 捆绑 ~1700 行 I2/I3/I4 真实执行 + NewAPI 适配器于 ML 闭环节点 T33**。默认：保持捆绑（框架 T20 原样，项目自身以"闭环"为单位）。备选：拆分集成内容至 T31——受依赖序限制风险高，不推荐。

### 风险与设计说明

- **T15 为 12-commit 聚合节点**（a330d14..aab0158 连续段），与 Stage 2 冻结语义一致；新 stage-2-v1 tag 的树与原 aab0158 树完全一致（同序应用同 diff）。
- **f0ea770 保留在 T33**（时间序位置）而非回移到其行数最多的冻结契约节点（T15/T16/T19 等）：回移会使新 stage tag 的中间树偏离原冻结树，且需跨节点重排（冲突风险）。T33 是它实际修改文件（agent_takeover.rs、ml/dataset.rs）的最近前驱节点。
- **全部 fixup/squash 保持原始相对顺序**，唯一重排是 2a18749（裁决 #8），最小化冲突面。
- **作者与日期**：单作者；每个新节点保留锚点 commit 的 author date，节点序与时间序一致（重排后 T03 日期 16:20 仍晚于 T02 锚点 14:07、早于 T04 锚点 17:09），日期单调。
- **消息规范**：所有 subject 符合 ^(feat|fix|refactor)\((core|router|protocol|policy|runtime|ml|account|provider|integration|tauri)\): [a-z] 格式；无 Stage/Gate/Roadmap/测试数量/closure/final 字样。

### Stage Tag 锚定（供 H6 使用）

| TAG | 目标节点 | 说明 |
|---|---|---|
| stage-1-v1 | T14 | 命名样式与能力契约 |
| stage-2-v1 | T15 | 协议与 IR 冻结 |
| stage-3-v1 | T18 | 策略运行时 + 决策追踪 |
| stage-4-v1 | T22 | 观测 + 打分 + 失败语义 + 统计 |
| stage-5-v1 | T23 | 账号组件 |
| stage-6-v1 | T24 | Outcome/Feedback |
| stage-7a-v1 | T25 | 特征提取 |
| stage-7b-v1 | T29 | 训练数据集 |
| stage-7c-v1 | T30 | 决策模型 |
| stage-7d-v1 | T32 | 评估框架 |
| stage-7e0-v1 | T34 | 模型身份与重放 |
