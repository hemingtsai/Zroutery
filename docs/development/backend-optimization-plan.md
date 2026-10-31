# 后端优化计划（GPT 6.1 SOL 执行）

> 与 [`ui-optimization-plan.md`](ui-optimization-plan.md) 同一战役的后端面；前端面与协作约定（契约流程、验收流水线、设计系统守护）都在那边，此处不重复。范围来源：[`upgrade-master-plan.md`](upgrade-master-plan.md)。研究依据：[`cc-switch-design-notes.md`](cc-switch-design-notes.md)。

## T · 托盘快切面板 🔄（Rust 窗口/ACL/命令已交付，前端面板与真机交互待验）

- 托盘左键弹出轻面板（独立透明 webview，参照 cc-switch `src-tauri/src/tray_panel.rs` + `src/tray-panel/`）：当前 tier 路由 + 提供商启停 + 最近请求摘要
- 交互规范（notes §5）：**50ms 悬停展开 + 鼠标轨迹方向感知**（3 点 trail 判意图，防相邻误开）；切换后面板不关，反馈写入面板顶部；子面板打开清空搜索
- 前端面板页由 ZCode 在 ui/ 内实现（tray-panel 模式同构复用主窗组件）；Rust 侧需要：托盘左键事件 → AttachWindow、面板窗口管理、（若现有命令不足）增量命令
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

## B5 · 用量数据面增量 ✅（聚合、日成本/缓存、SQL 分页已验证）

- `StatsSummary` 增加 cache_read/cache_write_tokens 聚合（前端 U3 KPI 现用 per_model.cached_tokens 聚合顶替）
- `get_usage_days` 增加 cost/cache 列（前端 U3 已论证：日账本没有这两列，"cost 视角"就是编造——补上后 KPI 五格全部可驱动图表）
- 若日历史要分页：按 page/pageSize/total 接口加 SQL 查询（前端 U8 翻页器换源不动 UI）

## B6 · 导出为文件 ✅（命令与文件回归通过，UI 接线待前端）

- 用量历史导出现在是 textarea + 复制；加 Tauri 保存对话框走真实文件（前端 U15' 等待此能力）

## B7 · 系统强调色注入 ✅（原生命令/ACL/契约已交付，3 项测试通过；前端待接线）

- **平台矩阵**（CSS `AccentColor` 通道不可靠）：Windows/WebView2 ✓ 映射 DWM；macOS/WKWebView ✗ WebKit 反指纹故意伪装成默认蓝（issue #136）；Linux/WebKitGTK ✗ 无统一来源
- **解法**（magpie omarchy.go:241-293 实证）：Rust per-platform 读原生 accent → 命令返回 `{ accent, accentSoft, accentFg }`（on-primary 由后端按 YIQ 亮度算好）→ 前端注入 `--system-accent` 系变量，`--primary: var(--system-accent, 回退色)`
- Windows：DWM `ColorizationColor`（registry，ABGR）或 WinRT UISettings；macOS：`NSColor.controlAccentColor`；Linux：尽力而为（GNOME 47+ gsettings accent-color、KDE kdeglobals、其余回退）
- M3 双主题（MD3/Fluent）与未来任何 skin 都消费这三个变量；语义色不跟随系统

## B2 · 契约变更流程（不变）

snake_case；**先改 `ui/src/api.ts` 类型（契约源）→ 实现 Rust → 同步 mock-bridge fixture（已类型化，编译器拦漂移）**。已知缺口示例：`RequestRecord.client` 曾缺声明（E-184 已补）。
