# UI/UX 优化整合计划

> 分工：**前端 = ZCode**（ui/ 全部），**后端 = GPT 6.1 SOL**（crates/ + src-tauri）。
> 状态：✅ 已交付（未提交） · 🔄 进行中 · ⬜ 待办。标签 [F] 前端 / [B] 后端 / [F+B] 契约协作。
> 依据：2026-10-31 全量 UI 审计 + cc-switch / magpie 代码研究（模式要点见文末附录）。

## 已交付 ✅

| 项 | 内容 | 位置 |
|---|---|---|
| 工程防护 | mock-bridge 纳入 tsc + `appConfig(): AppConfig` 类型化；9 处 fixture 漂移全修（wallet_holds / inference_keys / balances / rectifier / codex_guardian_signature / portable_history / decision / provider-model 形状 / per_client + get_usage_days 命令）；heatmapWeeks 空数组防护 | [F] ui/mock-bridge.ts、ui/src/pages/UsageCharts.tsx |
| 池拖拽重排 | RoutePool 整行拖拽 + ↑↓ 键盘等价；`applyTierOrder` 重写该层 priority（10/20/30 间隔）；策略诚实提示（非 Priority 显示"实际顺序由策略决定"）；插入线用 box-shadow 防命中抖动 | [F] ui/src/pages/Routing.tsx、ui/src/api.ts |
| 保存状态行 | SaveStatusContext（saving/saved·时刻/失败）+ Drawer 脚部自动携带 | [F] ui/src/saveQueue.ts、components.tsx、App.tsx |
| 时长三档 | `ms()`：<1s→ms，<60s→s，≥60s→`4m 28s`，防御 h 档；Overview 裸渲染收编；MlRouting 的 latency_regression 刻意保留裸 ms（测量差值非等待时长） | [F] ui/src/api.ts、pages/Overview.tsx |
| i18n 欠账 | `common.working` / `app.more_notifications` / `app.fewer_notifications` 三语补齐，删 App.tsx 硬编码英文 | [F] ui/src/App.tsx、i18n.tsx、i18n.zhTW.ts |
| mock 保真 | save_config 回显 + get_snapshot 用 sessionStorage 持久化（模块级变量会被整页刷新重置） | [F] ui/mock-bridge.ts |

## 批次 U · 用量页可读性 🔄 [F]（当前批次）

数据语义（已核实 stats.rs:124）：**`input_tokens` 是 cache-包含总数**，cache_read/write 是其子集。
缓存命中率 = `cache_read ÷ input_tokens`；fresh input = input − read − write。

- ✅ U1 价格单元格 in/out 标注：`输入 3 / 输出 15 · USD/百万`（en `in 3 / out 15 · USD/1M`）；纯符号版 priceText 已删（全站仅一个调用点，改在调用处用 t() 出三语标注）
- ✅ U2 热力图尊重范围选择器（7/30/90 + 新增 365 档）；画布宽度本就按周数自适应，年轻安装画 4 周而非一年灰海
- ⬜ U3 KPI 行（设计已升级，见 docs/development/cc-switch-design-notes.md §1）：五个 KPI **可点击**，选中即切换下方流量图指标；缩写值+title 精确值；命中率问号写口径（cache_read÷input）；上游不报缓存写显示 N/A（三态）。缓存命率先用 `per_model.cached_tokens` 前端聚合
- ⬜ U10 Toast 去重（notes §3）：ToastProvider 加 53 行式同文案原地刷新，调用方零改动
- ⬜ U11 请求日志分组间距 + tab 计数（notes §2/§7）：列表列按 身份/成本/Token 分组留距；Activity 三个 tab 名带条数
- ⬜ M1 补充（notes §6）：whats-new 按版本 JSON（三语内联）+ 首启/升级首见弹窗 + 设置关于区
- ⬜ H2 补充（notes §4）：行内余额列定宽右对齐；仅当前抽屉打开的提供商自动刷新余额
- ✅ U4 持久用量历史客户端表分层：名字自我复制修复（未知客户端="未知客户端"+截断 id，完整值 title）；数字/花费列 nowrap；1 次请求全失败无花费的零星来源折进"其他零星来源（n）"Disclosure（清除按钮保留在折叠区内）；导出/导入移入段头 actions 槽（见 U9）
- ✅ U5(部分) 失败筛选：列表上方 全部/仅失败 Segment；切换重置页码；空集有专属文案。失败计数→点击下钻仍开放
- ✅ U8 请求列表分页（cc-switch RequestLogTable 模式转化）：每页 50、`第 x–y 条 · 共 N 条` 翻页器（单页隐藏）、越界自动收回、2 秒轮询下分页稳定。数据源仍是内存 recent 窗口（log_limit）；**[B] 若日历史要翻页，按 page/pageSize/total 接口加 SQL 查询，前端换源不动 UI**
- ✅ U9 动作归位（cc-switch 页头动作模式）：导出/导入 → 段头 actions 槽；添加提供商/添加模型/添加预算表单默认折叠，入口=页头/段头首动作（common.collapse 收起）；添加成功自动收起
- ✅ U7 图表悬浮卡：单例 portal 卡（fixed 层挂 body——**新约定：tab-panel 的 page-in 动画 transform 会劫持 position:fixed，任何固定层必须 portal**）；键盘取舍=格子不可聚焦（371 只读格不进 Tab 序），无指针读取将来走数据表形态
- ⬜ U7 跟进：柱状图 >30 天时条宽 1px 悬停难命中（Line 已有透明命中矩形先例，照搬）
- ✅ U12 用量页重构：一个账本、两种切法（按客户端/按模型 Segment 同源 GROUP BY）+ 点行筛流水（与失败筛选叠加、可关闭提示条）+ 健康降级为按模型行内状态点；持久历史/花费/模型健康/持久结算四节消亡
- ✅ U14 请求流水 facet 筛选（cc-switch UsageDashboard.tsx:186-194 模式）：供应商 Select（选项带计数 OpenCode · 3,398）+ 级联模型 Select；纯前端 Map 计数；与仅失败/账本行筛选叠加（后动者优先互斥，E-186）
- ✅ P2 头号候选落地：Models 列表分页（cc-switch TablePagination 模式）——热切换 236.8ms → 50.6ms（4.7×），翻页 17-18ms，搜索全集过滤后再分页（E-188）
- ✅ U16 流水行 首字 + 速度（tok/s）两格 + 抽屉命中率——公式与门槛照抄 cc-switch format.ts（E-189）；行级读 final-attempt usage 的口径细节已记录
- 🔄 U17 接线 [B5]/[B6]（E-193）：命中率改后端聚合、图表加缓存视角、导出走原生文件对话框（取消静默/失败回落 textarea）、花费 KPI 带未定价计数；后端已有按币种的日成本列，cost 日视图和 SQL 分页仍待前端接线
- ⬜ [B7] 系统强调色注入落地后：MD3/Fluent 皮肤 `--primary` 切 `var(--system-accent)`（M3 已留回退链）
- ⬜ 字体首绘（P2 第三候选：CJK 分片 262KB）；@fontsource/ibm-plex-sans 零产物清理
- ✅ U15' 导出为文件：已随 U17 接入原生保存对话框，保留 JSON textarea+复制作为失败回退（E-193）
- ⬜ U6 内部日志空态对齐 Empty；"持久结算/8 次中断尝试待核对"句式重写
- ⬜ [B·可选] StatsSummary 增加 cache_read/write_tokens 聚合（前端先用 per_model 聚合顶上，非阻塞）

## 批次 H · 手感下半场 ⬜ [F]

- ⬜ H2 行内直控：提供商行/模型行启用开关（点 = toggle + save）
- ⬜ H3 乐观 UI：拖拽先落再存，失败回滚 + 行内提示（magpie 范式：`field.value = was`）

- ✅ 提前完成（原批次 2.5）：添加提供商/添加模型/添加预算三处常驻表单全部折叠，入口=页头/段头首动作；布局桩新增 `unfold` 语义（点页头第 N 个 linky 展开后测量），providers/models surface 加 unfold=0

## 批次 M · 双模式（简易 / 极客）✅ M1 已落地 · M2 待办 [F]

铁律：模式只过滤可见性，永不改变行为；存 localStorage（与主题同机制），不进 config。

- ✅ M1 机制 + 导航收敛（simple 3 项：概览/用量与记录/设置）+ 提供商抽屉折至 事实区/密钥/启用/移除 + deep link 回落 + 设置页模式行与关于区 + 侧栏底部切换 Segment + Alt+数字改读可见导航 + 一次性高级配置 toast + zh 提供商→服务商 68 处统一 + 桩钉 geek 模式
- ⬜ M1 跟进：zhTW task.catalogue 服務商→供應商 统一；ToastProvider 增中性 tone（deep link 提示暂用 ok）
- ⬜ M2 首页合成（网关行 + tier 池可拖 + 提供商精简列表 + 最近请求）+ 预算只读条 + 目录添加默认 standard 层（简易模式；极客保持不猜）+ 布局桩新增简易 surface
- 切换体验：首切简易且检测到高级配置生效 → 一次性 toast "N 项高级配置正在生效，简易模式不显示它们"
- 术语：zh 简易模式/极客模式；en Simple/Advanced

## 批次 P · 总计划继承的前端工作 ⬜ [F]

> 来源：`upgrade-master-plan.md`（U 产品清单 / R 工程 / T 性能与 UI 三源整合）。
> 按其规则 1 去重："同一交付目标只做一次"——下表把总计划的前端面收编进本计划，
> 执行时与 `upgrade-contracts.md` / `upgrade-execution.md` 对齐（执行记录：M4 未开始，本节全部待办）。

### P1 · P0 三项的 UI 面 [F+B]（后端由 GPT 6.1 SOL 修，UI 面我接）

- P-ACCOUNT：提供方余额行标"部分数据"——已探测账户数、成功/失败数、数据时间（master §4 要求 5）；失败账户不当零
- P-PRICE：价格来源/优先级/新鲜度可见（人工定价不被目录刷新静默覆盖）；未知、缺字段、过期显式显示（P-PRICE 4）
- P-EFFORT：模型页/目录导入可选档位与实际发送值同源；不支持档位明确拒绝，不展示原档位（P-EFFORT 4-5）

### P2 · 前端性能（T B1-B7 + M0 基线）

- 基线先行（master 规则：未测量不优化）：600 模型 / 500 活动记录 / 30 账户 / 820×560，测分包、首屏、交互延迟、滚动帧率、字体实际传输（§3 前端基线）
- B1 配置（低频）/活动（高频）更新分离、稳定 memo 依赖、隐藏页停轮询与返回补读（现状：visibility 轮询已有，查重叠/去重缺口）
- B2 snapshot/activity 版本增量、请求去重、目录加载限并发与取消、稳定 modelRows/ID 映射
- B3 Models 目录 >200 行虚拟化/过滤验证，600 行基准（B3 与 U8 分页互补：列表分页已做，目录流另测）
- B4 Drawer 焦点只顶层捕获（已有）、Popover 边界翻转、**Toast 定时器在删除与卸载时清理**
- B5 三语按需加载（繁中已按需模块，查剩余差距）
- B6 页面懒加载（已有 lazy）+ 分包预算（react-vendor/vendor 已分），补包体报告
- B7 字体 unicode-range/字重/font-display 与 CJK 字形覆盖核对

### P3 · 页面与可访问性验收（T C1-C8）

- C2：820×560 无页面级横溢出（双滚动条根因已修，残余逐页验）；表格自有横滚；窄窗抽屉可用
- C4 逐页清单照 master §7 原文过一遍（Overview 长尾路由/Providers 余额一致/Models 长 ID 与过滤后选择/Accounts 主次/Activity 空态/Settings 网关分组/Takeover 长文本复制/Cutover 状态标签）——审计已覆盖大半，残余并入各批次
- C5/C6：焦点恢复、Esc 顶层关闭（已有 overlay 栈）、触控目标、table scope/caption、aria-live、reduced-motion、保持 RadixSelect 可访问性
- C7：统一加载/空/错误态（并入 U6）
- C1/C8：语义 token 梳理（为 P5 主题铺路）

### P4 · 产品功能 UI（U 编号的界面面）

- U1 多客户端识别显示与图标（等 M2 数据层，[F+B]）
- U1.5 CCSwitch 历史导入的映射/预览/幂等反馈/失败不部分写入的 UI
- U4 简化导入流程（与 U1.5 共用预览组件）
- U5 新手引导：首次配置→凭据→模型→路由→客户端→验证闭环，可跳过/重进
- U7 NewAPI 条件显示：配置额度探测后才展示面板相关功能，不把所有 OpenAI 兼容当 NewAPI
- U8 签到状态机 UI：登录失效→提示手动登录→成功后刷新余额；独立签到页与残留清理提示
- U12 OAuth 授权入口（用户触发；刷新令牌留凭据库）
- U3 多 Key 账户 UI：备注、优先级、每 Key 余额（若支持）
- U2 托盘菜单 = 批次 T（已列，后端为主）

### P5 · 主题系统（U15）✅ = DAG 节点 UI-UX-M3（DONE，E-191+E-195；dark 反转修正与跟进见下）

- **skin × theme 双轴**：`data-skin`（industrial=现状默认 / md3 / fluent）× `data-theme`（light/dark 已有）；持久化 `zroutery-skin`；设置外观区加皮肤 Select
- **token 三层**：层1 全局语义 token 全 skin 共享（组件只消费这些）；层2 每 skin×theme 一份值；层3 组件覆写只做 token 表达不了的形状
- **四条设计决定**：`--control-h` 30px 跨 skin 恒定（布局桩有效性边界）；图表墨色 color-mix(--text) 自动随 skin 调 tint（不加图表 token）；字体按 skin（industrial=Noto Serif SC 现状 / md3=Roboto+Noto Sans SC / fluent=Segoe UI Variable+雅黑——与 P2 字体首绘优化合并处理）；fluent accent 需加深才作文本色（对比度 ≥4.5:1 全六组合记录）
- **MD3 视觉**：圆角 12/28、surface tonal tint；**Fluent 视觉**：圆角 4/8、acrylic 侧栏、深度阴影
- **组件形态矩阵**（用户裁决"md3不md3 fluent不fluent"后解除最小覆写约束）：MD3=药丸按钮/tonal 表面/M3 segmented 胶囊/侧栏全宽 pill/drawer 起始圆角 16px/KPI tonal tiles；Fluent=acrylic 侧栏（blur+透明）/4px 控件/Fluent 双环焦点/toast infobar 色条/深度阴影 flyout；`--control-h` 与布局桩有效性不变；industrial 零改动
- **强调色用系统的**（用户指示）：`--primary: AccentColor` + `--on-primary: AccentColorText`（CSS 系统颜色，WebView2 映射 Windows DWM 强调色；硬编码值仅作回退）；hover 用 color-mix 随主题方向；MD3 与 Fluent **共用**系统强调色，差异留在圆角/材质；状态语义色不用系统色；极浅自定义 accent 的文本对比度与 OS 自身按钮行为一致（记录 caveat）
- **守住**：`--control-h` 恒定=布局桩有效；geek/industrial 现状零改动
- ⬜ M3 跟进：Fluent light `--line #E0E0E0` 非文本对比 1.6:1（低于 WCAG 1.4.11 的 3:1——Fluent 原生取向与可访问性的设计裁决待定）；Popover 菜单未进组件矩阵（方角残留）；字体按 skin（与字体首绘合并）

### 去重对照（总计划 ↔ 本计划）

| 总计划项 | 本计划状态 |
|---|---|
| T B3 Activity 分页 | ✅ U8 已做 |
| T C7 统一空态 | ≈ U6（待办） |
| T B4 Toast 清理 | 并入 U10 邻近（待办） |
| U2 托盘 | = 批次 T（已列） |
| U15 主题 / U5 引导 / U1.5 导入面 | = P5 / P4（本节新增） |
| 审计发现（ms/价格/术语/抽屉…） | 不在总计划内，本计划原生（U1-U11、H、M） |

## 批次 T · 托盘快切 / 后端待办 ⬜ → **已拆至 [`backend-optimization-plan.md`](backend-optimization-plan.md)**（GPT 6.1 SOL 执行：T 托盘、B1 事务化、B3 指纹补录、B4 接管扩张、B5 用量数据面、B6 导出文件、B2 契约流程）

## 协作约定

0. 本计划与 `upgrade-master-plan.md`（三源总计划）的关系：总计划是范围来源，本计划是前端执行入口；其 M0 基线（先测量后优化）与"代码已有/已验证/已提交三态不互代"规则照单全收。执行细节以其 `upgrade-contracts.md` / `upgrade-execution.md` 为准。
1. 前端不碰 `crates/` `src-tauri/`；后端不碰 `ui/`。
2. 契约变更流程：api.ts 类型 → Rust 实现 → mock fixture → 双方各自 build 验证。
3. UI 结构性改动同 commit 更新 `scripts/ui_layout_test.py`（桩按 Surface 声明，本机无 Python，CI 跑）。
4. 验收流水线：`pnpm --dir ui build`（tsc）→ `pnpm --dir ui test` → mock 实操作验证 → 真机只读目视（活配置只准读）。
5. 设计系统守护：不做卡片/导航图标/彩色徽章/圆角/阴影/动效加料；一切以"是否服务于信息结构"为准（ui/src/styles.css 头注）。
6. 文案三语同步：en/zh（i18n.tsx）+ zhTW（i18n.zhTW.ts），键集由 tsc 强制一致。

## 附录 · 双参照研究要点

- **cc-switch**（Tauri，farion1231/cc-switch）：切换一等公民+状态位定宽防跳；托盘 3 击切换且面板不关；toast 原地去重；事务写入（pending/staged/发布）；反面对照：图标按钮堆砌、巨石表单（3153 行）、用量页默认藏列。
- **magpie**（Go/Wails，yetone/magpie，**同类竞品**）：行内点击即生效（无保存键）、乐观更新失败回滚、模型选择器带实测 tok/s/TTFT、失败计数可点下钻到厂商原话与路由决策解释；LESSONS.md 铁律："布局是产品意志，不做无诉求重排"、"默认永不覆盖用户设定"；反面对照：24,585 行单文件前端、托盘菜单不能直切。
- 两者收敛的顺滑公式：**行内即时操作 / 反馈贴身 / 数据上浮到决策现场 / 当前态永远可见 / 计数到诊断连续路径**。
