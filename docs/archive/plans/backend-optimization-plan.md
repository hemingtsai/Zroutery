# 后端优化计划（GPT 6.1 SOL 执行）

> 与 [`ui-optimization-plan.md`](ui-optimization-plan.md) 同一战役的后端面；前端面与协作约定（契约流程、验收流水线、设计系统守护）都在那边，此处不重复。范围来源：[`upgrade-master-plan.md`](upgrade-master-plan.md)。研究依据：[`cc-switch-design-notes.md`](../records/cc-switch-design-notes.md)。

> 2026-11-01 状态对账（只读核对，`dev` HEAD/origin/dev=`4075a54`，工作区大量未提交改动）：本文件已更正 B4 的 Gemini 入口精度、B5/B6 的 UI 接线状态与 B7 的 DWM 字节序。E-192 的 3,238 全量绿是 2026-10-31 的带日期记录；其后新增工作由 E-196~E-203 的聚焦门禁覆盖，不得读作当前全量绿。剩余后端项的源码定位见 [`backend-optimization-execution.md`](../records/backend-optimization-execution.md) 的对账节。

## T · 托盘快切面板 🔄（Rust 窗口/ACL/命令已交付，前端面板与真机交互待验）

- 托盘左键弹出轻面板（独立透明 webview，参照 cc-switch `src-tauri/src/tray_panel.rs` + `src/tray-panel/`）：当前 tier 路由 + 提供商启停 + 最近请求摘要
- 交互规范（notes §5）：**50ms 悬停展开 + 鼠标轨迹方向感知**（3 点 trail 判意图，防相邻误开）；切换后面板不关，反馈写入面板顶部；子面板打开清空搜索
- 前端面板页由 ZCode 在 ui/ 内实现（tray-panel 模式同构复用主窗组件）；Rust 侧需要：托盘左键事件 → AttachWindow、面板窗口管理、（若现有命令不足）增量命令
- 2026-11-01 只读核对：UI 侧目前只有契约与桩——`ui/src/api.ts` 已声明 `trayPanelReady`/`hideTrayPanel`/`showDashboard`/`traySnapshot` 及 `TRAY_PANEL_WINDOW_LABEL/ROUTE`，`mock-bridge.ts` 有空桩；**没有 tray-panel 页面、也没有任何 `trayPanelReady()` 调用**。按当前源码，面板未在 2 秒内握手时回退到主窗（源码推断，`src-tauri/src/tray_panel.rs`；运行二进制/真机未复验）
- cc-switch 托盘反面对照：菜单本身不能直切模型（只有打开/退出）——我们的菜单至少要保留"打开面板"以外的直达项

## B1 · 配置写入事务化 ✅（多文件接管已接入，17 项事务故障恢复测试通过）

- staged 临时文件 → 逐个发布 → 崩溃前滚补完或整体丢弃（参照 cc-switch `src-tauri/src/mode/operation.rs:104-174`）
- 切走前 live 快照 + 用户手改内容回填（cc-switch `save_live_backup` 模式）
- 现状 saveQueue 已保证前端写入顺序；本项是 Rust 侧崩溃一致性

## B3 · 客户端指纹补录 ✅（48 项归因 + 14 项配置测试通过；8788 已加载修复构建，新流量归真名）

- 实际落地（前端负责人已核对 client.rs 源码）：`UA_RULES` 新增 `{ id: "codex_desktop", name: "Codex Desktop", family: Desktop, prefixes: ["codex_desktop_", "codex desktop/"], provenance: Observed }` ——双前缀覆盖下划线观测形态与常规 `Codex Desktop/<version>` 形态；顺带按 magpie 目录对抄 `pi-desktop`（置于 Pi 之前）
- 对表抄录 magpie `internal/agent/agents.go:146-161` 的 Known UA 目录（pi-desktop、kimi-code-desktop、raft 等）
- 用户现场数据：该"未知客户端"已 952 次 / 169 失败 / 416 USD——修完历史记录保持原 id（不重写身份），新请求开始归真名
- 前端无需改动：账本按 client_id 分组，新旧 id 并存是设计行为（历史不重写）

## B4 · 接管扩张 ✅（Gemini 原生网关与双文件适配已验证；UI/原生菜单待协同验收）

- **首推 Gemini CLI**：配置 JSON、magpie/cc-switch 都支持、Zroutery 网关已有 gemini codec——最低垂
- **中期：`edit.KV` 式声明化接管**（magpie `internal/agent/agents.go:296,387`）：接管 = 对客户端配置的一组 `{Path, Value}` 键路径声明（JSON/TOML 通吃）+ 发现即目录（Bin 在 PATH / 配置存在）——新客户端 = 声明而非代码
- 克制原则：magpie 60+ 客户端是护城河也是包袱（其 LESSONS："A fix reaches every sibling"）——只接真实在用的
- 前端协作面：takeover_status 的 agent 清单与新键（ZCode 负责 UI 面）
- 2026-11-01 只读核对（不要笼统说"无 Gemini 入口"）：`ui/src/pages/Takeover.tsx` 的 `AGENTS` **已含 `gemini`**（代理列表/状态行可见），但 `MENU_AGENTS` 仅 claude/codex/opencode——**设置菜单入口缺**；后端 field set 已要求显式 default（`crates/zroutery-core/src/agent_takeover/tests.rs::gemini_field_set_is_exact_and_requires_an_explicit_default`），UI 的显式默认档选择未接。原生 CLI 菜单/请求验收仍开放

## B5 · 用量数据面增量 ✅（后端聚合、日成本/缓存列、SQL 分页均已验证；日成本视图与翻页器 UI 未接，归前端）

- `StatsSummary` 增加 cache_read/cache_write_tokens/unpriced_requests 聚合，`UsageDay` 增加 cost/cache 列（`crates/zroutery-core/src/stats.rs`）。前端缓存聚合与「缓存」图表指标已由 U17 接线（E-193），不再用 per_model.cached_tokens 聚合顶替
- 历史分页后端已交付并注册：`query_usage_history({query})` 返回 `{records, page, page_size, total, revision}`（`src-tauri/src/commands.rs`、`src-tauri/src/state/budget.rs`），页大小默认 50、上限 200，`expected_revision` 防翻页竞态
- 仍归前端：日成本（按币种）视图与历史翻页器换源未接（E-193 原话 "its UI and SQL pagination remain unwired"——指 UI，不是后端缺）

## B6 · 导出为文件 ✅（命令与文件回归通过，前端已接原生保存对话框；WebView2 真机对话框目视验收待做）

- 前端导出已改走 `export_usage_history_file()` 原生保存对话框，textarea+复制保留为传统回退（E-193）；后端约束不变——成功 fsynced 原子写之后才返回路径、取消返回 `null`、webview 不接收目标路径或归档大小的载荷（见 execution 文档 B6 节）；E-198 明示原生对话框未在 WebView2 上驱动过

## B7 · 系统强调色注入 ✅（原生命令/ACL/契约及前端 CSS token 接线已交付；原生目视验收仍待完成）

- **平台矩阵**：Rust per-platform 读系统 accent；可用时 Tauri UI 调用 `get_system_accent` 并注入 `--system-accent`、`--system-accent-soft`、`--system-accent-fg`；返回 `null` 或 IPC 失败时使用皮肤 fallback。CSS `AccentColor` 不作为数据来源。
- **解法**（magpie omarchy.go:241-293 实证）：命令返回 `{ accent, accent_soft, accent_fg }`；前端把同一 native value 接入 MD3/Fluent，dark 继续做 tone inversion，语义状态色保持独立。
- Windows：DWM `DwmGetColorizationColor` 返回 **AARRGGBB**（非 COLORREF 的 BGR；`src-tauri/src/accent.rs` 实读即此）；原稿候选中的 registry/WinRT UISettings 未采用；macOS：`NSColor.controlAccentColor`；Linux：尽力而为（GNOME 47+ gsettings accent-color、KDE kdeglobals、其余回退）
- M3 双主题（MD3/Fluent）消费这三个变量；选择时同步持久化 skin/theme，初始化首帧前应用；语义色不跟随系统。UI 单测和 mock build 已通过，WebView2/native theme change 视觉验收仍单列

## B2 · 契约变更流程（不变）

snake_case；**先改 `ui/src/api.ts` 类型（契约源）→ 实现 Rust → 同步 mock-bridge fixture（已类型化，编译器拦漂移）**。已知缺口示例：`RequestRecord.client` 曾缺声明（E-184 已补）。
