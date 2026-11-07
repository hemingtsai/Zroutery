# UI/UX 优化整合计划

> 分工：**前端 = ZCode**（ui/ 全部），**后端 = GPT 6.1 SOL**（crates/ + src-tauri）。
> 状态：✅ 已交付 · 🔄 进行中 · ⬜ 待办（提交范围不一刀切，见下方状态口径）。标签 [F] 前端 / [B] 后端 / [F+B] 契约协作。
> 依据：2026-10-31 全量 UI 审计 + cc-switch / magpie 代码研究（模式要点见文末附录）。
> 状态口径（2026-11-01）：`dev` 与本地 `origin/dev` 均为 `4075a54`。多数早批交付已随 `72f5b1e`（双模式/用量账本/facet/皮肤）与 `4075a54`（用量视图接线/原生客户端）进入 Git；**E-196~E-203 等后续增量仍未提交**。下文按源码与 E-180~E-204 核对现状，历史门禁不是本轮重跑；总计划 M0~M6 与这里的 U/M/P 编号不是同一范围。当前合同入口为 `upgrade-contracts.md` 指向的已接受总计划，`upgrade-master-plan.md` 仅为历史草稿。

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
- ✅ U2 热力图尊重范围选择器（7/30/90 + 新增 365 档）；画布宽度本就按周数自适应，年轻安装画 4 周而非一年灰海。→ 2026-11-01 E-202 修订：热力图恢复**全年视图**并按面板实测宽度定格子（7–22 px）、月份标签移到底部、去掉周一/三/五行标签、图例移入工具栏右端、新增「指标 · 跨度 · 合计」读数行；热力图开启时隐藏范围选择器，全年无缓存的指标不再提供并回退请求数
- 🔄 U3 KPI 行：五格读数已实现（E-181/E-193，Activity.tsx），有缩写/title、cache_read÷input 口径提示和未定价计数。请求数与 Tokens 两格带 onSelect；成功率、命中率和费用为只读，图表自身另可选择缓存指标。原“五个 KPI 可点击”与上游不报缓存写时 N/A 三态要求保留为剩余项；隐藏全年无缓存指标、无输入时显示破折号不证明已区分未知缓存与实测零。成本逐日 UI 见 U17。命中率已改用后端 StatsSummary 聚合；节点 UI-UX-U3 的历史 DONE 仅对应 E-181 的已收窄交付范围
- ✅ U10 Toast 去重（notes §3）：`toastDedup.ts` + ToastProvider 同文案同 tone 原地刷新，调用方零改动；`toastDedup.test.ts` 覆盖（E-180）；中性 info tone 跟进见 E-188
- ⬜ U11 请求日志分组间距 + tab 计数（notes §2/§7）：列表列按 身份/成本/Token 分组留距；Activity 三个 tab 名带条数
- ⬜ M1 补充（notes §6）：whats-new 按版本 JSON（三语内联）+ 首启/升级首见弹窗（未做）；设置关于区已落（简易模式设置页，Settings.tsx `settings.about`）
- ⬜ H2 补充（notes §4）：行内余额列定宽右对齐；仅当前抽屉打开的提供商自动刷新余额
- ✅ U4 持久用量历史客户端表分层：名字自我复制修复（未知客户端="未知客户端"+截断 id，完整值 title）；数字/花费列 nowrap；1 次请求全失败无花费的零星来源折进"其他零星来源（n）"Disclosure（清除按钮保留在折叠区内）；导出/导入移入段头 actions 槽（见 U9）
- ✅ U5(部分) 失败筛选：列表上方 全部/仅失败 Segment；切换重置页码；空集有专属文案。失败计数→点击下钻仍开放
- ✅ U8 请求列表分页（cc-switch RequestLogTable 模式转化）：每页 50、`第 x–y 条 · 共 N 条` 翻页器（单页隐藏）、越界自动收回、2 秒轮询下分页稳定。数据源仍是内存 recent 窗口（log_limit）；**B5 已交付 SQL 查询后端，前端换源归 U17**
- ✅ U9 动作归位（cc-switch 页头动作模式）：导出/导入 → 段头 actions 槽；添加提供商/添加模型/添加预算表单默认折叠，入口=页头/段头首动作（common.collapse 收起）；添加成功自动收起
- ✅ U7 图表悬浮卡：单例 portal 卡（fixed 层挂 body——**新约定：tab-panel 的 page-in 动画 transform 会劫持 position:fixed，任何固定层必须 portal**）；键盘取舍=格子不可聚焦（371 只读格不进 Tab 序），无指针读取将来走数据表形态
- ✅ U7 跟进：柱状图 >30 天条宽悬停已修（E-188）——条下先铺全列透明命中矩形，保住条自身的 `:hover` 高亮
- ✅ U12 用量页重构：一个账本、两种切法（按客户端/按模型 Segment 同源 GROUP BY）+ 点行筛流水（与失败筛选叠加、可关闭提示条）+ 健康降级为按模型行内状态点；持久历史/花费/模型健康/持久结算四节消亡
- ✅ U14 请求流水 facet 筛选（cc-switch UsageDashboard.tsx:186-194 模式）：供应商 Select（选项带计数 OpenCode · 3,398）+ 级联模型 Select；纯前端 Map 计数；与仅失败/账本行筛选叠加（后动者优先互斥，E-186）
- ✅ P2 头号候选落地：Models 列表分页（cc-switch TablePagination 模式）——热切换 236.8ms → 50.6ms（4.7×），翻页 17-18ms，搜索全集过滤后再分页（E-188）
- ✅ U16 流水行 首字 + 速度（tok/s）两格 + 抽屉命中率——公式与门槛照抄 cc-switch format.ts（E-189）；行级读 final-attempt usage 的口径细节已记录
- 🔄 U17 接线 [B5]/[B6]（E-193）：命中率已改后端聚合、图表已加缓存视角（`Metric "cache"`）、导出已走原生文件对话框（取消静默/失败回落 textarea，E-193）、花费 KPI 已带未定价计数。**仍开放**：UsageDay 已有按币种日成本列、`query_usage_history`（page/page_size/total/revision）桥已存在于 api.ts:1948 + mock，但 Activity 请求流水仍读内存 `recent` 窗口——成本日视图与 SQL 分页换源未接
- 🔄 [B7] Native accent UI 已调用 `systemAccent()` 并注入 CSS token，系统值缺失时保留皮肤 fallback；主题/皮肤/accent 持久化已过真实文档重载用例（UI-29，E-196）；原生 WebView2 外观/系统主题变化与全六态对照验收待做
- 🔄 字体首绘：Industrial/MD3 按皮肤异步加载 Noto Serif SC/Roboto，Fluent 用系统 Segoe/雅黑 fallback（实现/build 记录见 E-196）；未安装 Noto Sans SC，以守住 CSS 预算；移除未使用的 @fontsource/ibm-plex-sans。当前策略的实际首绘传输待复测，E-185 的旧字体基线保留
- ✅ U15' 导出为文件：已随 U17 接入原生保存对话框，保留 JSON textarea+复制作为失败回退（E-193）
- 🔄 U6 内部日志空态对齐 Empty（未做：logPane 空态仍是裸 `<pre>`/按钮，未对齐 Empty）；"持久结算/8 次中断尝试待核对"句式已重写为诚实口径（`accounting.unreconciled`："被重启打断，最终花费未知"，待核对措辞已不存在于代码）
- ✅ [B·可选] StatsSummary cache_read/write_tokens 聚合已由后端落地（E-193），前端命中率直接消费 summary 聚合，per_model 顶替方案已弃用

## 批次 H · 手感下半场 ⬜ [F]

- ⬜ H2 行内直控：提供商行/模型行启用开关（点 = toggle + save）
- ⬜ H3 乐观 UI：拖拽先落再存，失败回滚 + 行内提示（magpie 范式：`field.value = was`）

- ✅ 提前完成（原批次 2.5）：添加提供商/添加模型/添加预算三处常驻表单全部折叠，入口=页头/段头首动作；布局桩新增 `unfold` 语义（点页头第 N 个 linky 展开后测量），providers/models surface 加 unfold=0

## 批次 M · 双模式（简易 / 极客）✅ M1/M2 已落地 [F]

铁律：模式只过滤可见性，永不改变行为；存 localStorage（与主题同机制），不进 config。

- ✅ M1 机制 + 导航收敛（simple 3 项：概览/用量与记录/设置）+ 提供商抽屉折至 事实区/密钥/启用/移除 + deep link 回落 + 设置页模式行与关于区 + 侧栏底部切换 Segment + Alt+数字改读可见导航 + 一次性高级配置 toast + zh 提供商→服务商 68 处统一 + 桩钉 geek 模式
- ✅ M1 跟进（E-188）：zhTW task.catalogue 已统一 供應商；ToastProvider 已增中性 info tone，deep link 提示已从 ok 换为 info
- ✅ M2 首页合成（E-190；节点 UI-UX-M2 DONE）：网关行、tier 池拖拽、提供商精简列表、最近请求、预算只读条和目录添加默认 standard 层全部落地；Simple Overview 布局 Surface（`overview-simple`）已声明进 `scripts/ui_layout_test.py`，完整布局门禁此后多轮通过 **11 surfaces / 70 controls**（E-196、E-198~E-203），溢出 9 页 820×560 全过——早前"Usage budgets tab 超时致验收未闭环"已被这些记录取代
- 切换体验：首切简易且检测到高级配置生效 → 一次性 toast "N 项高级配置正在生效，简易模式不显示它们"
- 术语：zh 简易模式/极客模式；en Simple/Advanced

## 批次 P · 总计划继承的前端工作 🔄 [F]

> 来源：`upgrade-master-plan.md`（U 产品清单 / R 工程 / T 性能与 UI 三源整合）。
> 按其规则 1 去重："同一交付目标只做一次"——下表把总计划的前端面收编进本计划，
> 执行时与 `upgrade-contracts.md` / `upgrade-execution.md` 对齐（2026-11-01 口径：P5 主题已实现（节点 REVALIDATE）、P4 的 U1.5 导入面已随 M2b 落地、P2 基线已测（E-185）且 U8/U10 对应项已完；**M4 里程碑整体仍未闭环**，未完项见下）。

### P1 · P0 三项的 UI 面 [F+B]（后端由 GPT 6.1 SOL 修，UI 面我接）

- P-ACCOUNT：🔄 **已接**：BalanceChip 的 `aggregate.partial` 徽章显示成功/参与账户数，AccountBindingReview 提供绑定预览/发布/丢弃。**剩余**：呈现 `BalanceStatus.checked_at` 与 `BalanceStatus.observed_at` 的最近尝试/有效观测区分，核对失败、过期、未验证及部分汇总的未知语义；完成绑定流程的真实 WebView2 和指定账户验收
- P-PRICE：价格来源/优先级/新鲜度可见（人工定价不被目录刷新静默覆盖）；未知、缺字段、过期显式显示（P-PRICE 4）
- P-EFFORT：模型页/目录导入可选档位与实际发送值同源；不支持档位明确拒绝，不展示原档位（P-EFFORT 4-5）

### P2 · 前端性能（T B1-B7 + M0 基线）

- 基线先行（master 规则：未测量不优化）：600 模型 / 500 活动记录 / 30 账户 / 820×560，测分包、首屏、交互延迟、滚动帧率、字体实际传输（§3 前端基线）
- B1 配置（低频）/活动（高频）更新分离、稳定 memo 依赖、隐藏页停轮询与返回补读（现状：visibility 轮询已有，查重叠/去重缺口）
- B2 snapshot/activity 版本增量、请求去重、目录加载限并发与取消、稳定 modelRows/ID 映射
- B3 Models 目录 >200 行虚拟化/过滤验证，600 行基准（B3 与 U8 分页互补：列表分页已做，目录流另测）
- B4 Drawer 焦点只顶层捕获（已有）、Popover 边界翻转；Toast 定时器在淘汰/删除/卸载时清理已有实现，见 components.tsx 与 upgrade-execution.md 的核心界面记录（去重另见 E-180）
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

> 编号口径：本节 U 编号是总计划 §5 的产品编号；**批次 U 的 U1/U3/U6/U7/U10 等是 2026-10-31 UI 审计流水号，同号不同义**（如批次 U10=Toast 去重，总计划 U10=主请求自动选模），勿互相当同一条目。

- U1 多客户端识别显示与图标 [F+B]——🔄 部分：per_client 账本与显示名查找已接（E-201：旧原始 UA 聚合时查产品名，family/confidence 不改写）；图标、来源筛选和统一历史查询仍开放
- U1.5 CCSwitch 历史导入的映射/预览/幂等反馈/失败不部分写入的 UI —— 🔄 **UI 闭环仅到 预览→确认→提交→报告→清除**（cc-switch + Claude Code 已接，E-198/199/200：抽屉 grammar、"已在本机找到/用过"列表、按 `message.id` 去重、未计价单列）。不据此推断：源库原生对话框验收、整批原子性证据均未做。⬜ 开放：范围/窗口选择、跨来源重复计数、单位/计量语义核对、导入历史与图表/导出同口径查询、导入 KPI/日曲线合并已接但客户端/模型明细仍只算本机（E-203，导入库无归因证据，界面已注明）、Codex/OpenCode 适配器、WebView2 原生对话框与真实来源库验收
- U4 简化导入流程（与 U1.5 共用预览组件）
- U5 新手引导：首次配置→凭据→模型→路由→客户端→验证闭环，可跳过/重进
- U7 NewAPI 条件显示：配置额度探测后才展示面板相关功能，不把所有 OpenAI 兼容当 NewAPI
- U8 签到状态机 UI：登录失效→提示手动登录→成功后刷新余额；独立签到页与残留清理提示
- U12 OAuth 授权入口（用户触发；刷新令牌留凭据库）
- U6 多 Key 账户 UI（本行原误写为 U3）：备注/优先级与拖拽/键盘排序已接，发送顺序及 UI-27 见 E-196；支持方的每 Key 余额仍缺。U3 钱包恢复另需经验证的自动解除，当前仅手动解除
- U2 托盘菜单 = 批次 T（已列，后端为主）

### P5 · 主题系统（U15）🔄 已实现，REVALIDATE = DAG 节点 UI-UX-M3（实现与历史局部验证 E-190/E-191/E-194/E-195；2026-11-01 状态审计 **E-204** 将节点转回 REVALIDATE——required gates 的全六态截图/对比与原生 accent 验收仍开放；dark 反转修正与跟进见下）

- **skin × theme 双轴**：`data-skin`（industrial=现状默认 / md3 / fluent）× `data-theme`（light/dark 已有）；持久化 `zroutery-skin`；设置外观区加皮肤 Select
- **token 三层**：层1 全局语义 token 全 skin 共享（组件只消费这些）；层2 每 skin×theme 一份值；层3 组件覆写只做 token 表达不了的形状
- **四条设计决定**：`--control-h` 30px 跨 skin 恒定（布局桩有效性边界）；图表墨色 color-mix(--text) 自动随 skin 调 tint（不加图表 token）；字体按 skin（industrial=Noto Serif SC / md3=Roboto+系统 CJK fallback / fluent=Segoe UI Variable+系统 CJK fallback——与 P2 字体首绘优化合并处理）；native accent 由系统桥接注入，文本色对比遵循用户 OS accent 选择 caveat
- **MD3 视觉**：圆角 12/28、surface tonal tint；**Fluent 视觉**：圆角 4/8、acrylic 侧栏、深度阴影
- **组件形态矩阵**（用户裁决"md3不md3 fluent不fluent"后解除最小覆写约束）：MD3=药丸按钮/tonal 表面/M3 segmented 胶囊/侧栏全宽 pill/drawer 起始圆角 16px/KPI tonal tiles；Fluent=acrylic 侧栏（blur+透明）/4px 控件/Fluent 双环焦点/toast infobar 色条/深度阴影 flyout；`--control-h` 与布局桩有效性不变；industrial 零改动
- **强调色用系统的**（用户指示）：`get_system_accent()` 返回 `{accent, accent_soft, accent_fg}`，UI 写入 `--system-accent*`；`null` 时使用官方品牌 fallback；hover 用 color-mix 随主题方向；MD3 与 Fluent **共用**系统强调色，差异留在圆角/材质；状态语义色不用系统色；极浅自定义 accent 的文本对比度与 OS 自身按钮行为一致（记录 caveat）
- **守住**：`--control-h` 恒定=布局桩有效；geek/industrial 现状零改动
- 🔄 M3 跟进：已裁决交互控件边界至少 3:1，纯装饰分隔线可保留官方弱对比；`--control-line` 与装饰 `--line` 已分离，Popover 已加 MD3/Fluent 形状，Native accent 已接线且偏好同步持久化（UI-29，E-196）。**节点 UI-UX-M3 已按 E-204 转回 REVALIDATE**：重验范围=三 skin × 亮暗全页截图/对比度对照与原生 WebView2 accent 验收；已实现内容与 E-191/E-194/E-195 历史局部验证保留

### 去重对照（总计划 ↔ 本计划）

| 总计划项 | 本计划状态 |
|---|---|
| T B4 Toast 清理 | ✅ 已实现（components.tsx / upgrade-execution.md；去重另见 E-180） |
| T C7 统一空态 | ≈ U6（空态部分待办） |
| T B3 Activity 分页 | ✅ U8 已做（客户端分页；SQL 换源仍在 U17） |
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
