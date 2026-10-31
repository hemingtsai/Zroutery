# cc-switch 设计细节精读笔记（第三轮，逐文件）

> 前两轮是广度（交互流/信息架构），本轮逐文件读实现，只记**可直接转化为 Zroutery 的具体设计**。
> 仓库：E:\zr-research\cc-switch（只读）。每条：细节 → 证据 → Zroutery 落点。

## 1. KPI 卡是图表的控制器，不只是显示 🌟

`UsageHero.tsx:269-310`：每张 MetricCell 带 `selected={metric === "cost"}` / `onSelect={() => onMetricChange("cost")}` —— **点"总成本"卡，下面的图表就切到成本视图**。KPI 行从"读数"升级为"图表的档位"。

配套细节：
- **精确值进 title**：卡片显示 `14.4B`，悬停 title 给全数（`title={exact(fmtInt(...))}`，UsageHero.tsx:289）
- **口径解释进问号**：`help: {title, body}` —— 缓存命中率怎么算的写在"?"里（UsageHero.tsx:292-296）
- **N/A 三态**：`cacheWriteState: "na" | "partial"` —— 上游没报缓存写时显示 N/A 而不是 0（UsageHero.tsx:260-266, 347-351），诚实显示"不可知"与"零"的区别
- **"更多指标"展开**：`grid-rows-[0fr]↔[1fr]` 高度过渡 + 收起时整行 `inert`（展开区里的"?"不进 Tab 序，UsageHero.tsx:315-322）；展开按钮压在面板分隔线正中

**Zroutery 落点（U3 KPI 行的完整设计）**：五个 KPI（请求数/成功率/缓存命中/Tokens/花费）每个可选，选中即切换下方流量图的 metric（请求数↔Token 扩展为 成本/命中/Token）；标题行显示缩写值+title 全数；命中率问号写公式（cache_read÷input，stats.rs:124 语义）；上游不报缓存写时显示 N/A。

## 2. 请求日志列模型（requestLogColumns.ts，137 行教科书）

- **声明式列序**：数组顺序即表格列序，`as const satisfies` 类型化（:9-53）
- **locked 列**：时间列是打开详情的入口，不允许关（:10）
- **分组靠间距不画线**：`groupStart` 标记，换组处多留一段间距（:113-126）；**组内第一列被关掉时由同组下一列接上**（`requestLogGroupBreaks` 动态算，:49-59）
- **设置只存用户改过的列**：持久化 id→是否显示，没写的按默认值 —— "以后新加列不会被老用户的设置冻住"（:7 注释）
- **智能固定列**：横向滚动时前两列里 time/app 从第一列起连续固定，中间被关就少固定一列（:86-104）

**Zroutery 落点**：V2 的请求日志列显隐照此实现；近期先抄"分组间距"思路到现有列表（身份/成本/Token 三组）。

## 3. Toast 去重（toast.ts，53 行）🌟 最小成本最高质感

`key = kind:message`，同一句话还在屏上时再触发 → **复用 sonner 的 id 原地刷新、计时重来**，不叠摞（反复点"检查更新"不再堆一摞一样的）。业务代码无感知（统一从 lib/toast import）。

**Zroutery 落点**：components.tsx 的 ToastProvider 加同款 Map 去重（~30 行）；`notify("error", ...)` 的调用方零改动。

## 4. 行解剖防翻车细节（ProviderCard.tsx）

- **拖拽把手悬停显隐**：`h-6 w-4 opacity-0 group-hover:opacity-100 focus-visible:opacity-100`（:351-357）——键盘焦点也显。与我们池行 ↑↓ 的做法同构（已做 ✓，方向确认）
- **图标盒固定白底**：`bg-white text-neutral-900`，注释："单色（currentColor）图标和首字母 fallback 不能继承深色模式下的浅色文字"（:374-376）——任何按 favicon/首字母渲染的盒子都该如此
- **余额列定宽右对齐**：136px（带重置倒计时 194px），`max-w-[208px] text-end`（:527-529）——定宽保证切换卡片时布局零跳动
- **余额刷新策略**："每张卡按自己的号查，不只是正在用的那张"，但**只有当前卡按间隔自动刷新**（默认 5 分钟，ClaudeAccountQuotaFooter.tsx:24-27）——全量自动刷新是请求轰炸，当前卡自动+其余手动是对的
- **动作位+状态位定宽**（min-w-20 右对齐）——任何行切换后列表零位移

**Zroutery 落点**：provider 行内联余额（批次 H2）照此：定宽列+右对齐+当前抽屉打开的那家才自动刷；新做的 StatusDot 行内状态词已同构。

## 5. 托盘面板的手感工程（TrayPanel.tsx）

- **50ms 悬停展开**（HOVER_OPEN_MS），且**鼠标轨迹方向感知**：记最近 3 个鼠标点判意图（trail，:790-798），从子面板斜移回列表不会误开相邻项（:820-831 `aimedAt`）——悬停误触是这个组件形态的头号敌人，他们用轨迹解决了
- **切换后面板不关**，顶部插一行反馈"已切换到 X"（:1054-1058），problem 行可点击（:1050-1053）
- 子面板打开时**清空搜索框**（openSub 里 setQuery("")，:787）
- 面板内第二 tab 是用量摘要（UsageTab），与主窗同源

**Zroutery 落点**：批次 T（托盘面板，GPT 6.1 SOL 的 Rust 工作 + 我的前端 panel 页）直接按此实现：50ms 定时+轨迹判向+切换不关+顶部反馈行。

## 6. whats-new / 关于（M1 的设计输入）

- `src/whats-new/<version>.json`：每版本一个文件，`{version, items:[{type: new|improve|fix, zh, zh-TW, en, ja}]}` —— 变更日志是**数据**（四语内联），不是手写界面
- 展示三件套：`FirstRunNoticeDialog`（首启）+ `WhatsNewDialog`（升级后首见）+ `AboutSection`（设置内常驻）

**Zroutery 落点（M1 关于区）**：`ui/src/whats-new/<version>.json` 同构（zh/zh-TW/en 三语）；设置页"关于"区列当前版本要点；升级后首启弹一次。

## 7. 其他值得记的一手细节

- **禁用原因卡**：图标按钮禁用时不挂 tooltip 而换挂 DisabledReason 说明卡，两者不叠加（ProviderCardActions.tsx:247-265）——"为什么灰"比"能不能点"更需要版面
- **编辑当前供应商时，页头注明写入哪个 live 文件、"保存后立即生效"**（EditProviderDialog.tsx:42-53）——写目标透明化；Zroutery 的等价物：抽屉编辑时注明"写入 config.toml · 保存即生效"（我们已有 save foot，补"写入哪里"一句）
- **内联校验优先**：字段错误 `FormMessage` 内联在字段下，只有跨字段/重复 key 走 toast（ProviderForm.tsx:2393）——与我们一致，保持
- **Tab 计数徽章**：请求日志 60,458 / 供应商 9 / 模型 48 —— tab 名即统计，用户不用点进去才知道规模（UsageDashboard.tsx:101）

**Zroutery 落点**：Activity 的 请求与用量/内部日志/限额 tab 加计数；Providers 页 13 的数字已在 lede，保持。

## 8. 不抄（重申并补充）

- 17 个 QuotaFooter 组件（每上游一个）——Zroutery 单一余额探针抽象已覆盖
- ProviderForm.tsx 3153 行巨石表单
- 卡片整卡不可点、纯图标动作靠悬停（可发现性差）——我们行整行可点+文字动作
- react-hook-form + zod 重依赖——我们的 CommitInput/saveQueue 模式更轻

## 11. 接管目录对比（第六轮：更多客户端的接管支持怎么做）

**cc-switch：14 个 AppType**（app_config.rs:466-481：Claude、ClaudeDesktop、Codex、Gemini、GrokBuild、OpenCode、OpenClaw、Hermes、Pi、Mcode、Kimi、Dsh、Agy、Omp）。每 AppType = 一条代理路由前缀 + 一个 live 配置写入器（claude settings.json / codex {auth,config.toml} / claude_desktop 专列）+ 一个会话解析器（session_usage_*.rs）。接入成本 ≈ 三件套 × 客户端数。

**magpie：132 个集成文件（非测试）**，每客户端一个 Go 文件（claude/codex/gemini/goose/cursor/copilot/qoder/qwen/droid/cline/kimi/minimax/dsh/agy/pi 系/morph/hermes/muse/…）。三个可移植的抽象：
1. **`edit.KV` 键路径编辑器**（agents.go:296,387）：接管 = 对 app 配置文件的一组 `{Path, Value}` 声明（JSON 与 TOML 通吃），"只改该改的那一个键"落地为通用机制——**新客户端 = 一份声明，不是一段代码**。
2. **发现即目录**：`local()/onMachine()` 按 Bin 在 PATH / 配置文件存在来枚举机器上的客户端（agents.go:204-211）；piLike 模式把"Pi 及其全部 fork（保持 settings.json 形状）"识别为一个家族（:1292-1325）。
3. **UA 目录与代理归因同构**：agents.go:146-161 `Known{ID, Names, UA[]}`（含 pi-desktop、kimi-code-desktop）——跟 Zroutery client.rs 指纹规则表是同一物种，可直接对表抄录。

**协议层是真正的护城河**（app.js:9152 `PROTOS`）：provider 声明会说哪五种协议（chat/responses/**anthropic**/**gemini**/decide），agent 声明说哪种，网关做翻译——"任意 agent × 任意模型"由此而来。Zroutery 已有同等翻译层（anthropic/openai/gemini codec + api_surface），**所以 Zroutery 扩张接管的成本不在协议，在每客户端的配置发现与改写**。

**Zroutery 扩张路径（按成本升序）**：
1. Gemini CLI——两项目都支持；配置 JSON、网关已有 gemini codec，最低垂。
2. `edit.KV` 式声明化接管——把"每客户端一个 writer"重构成"每客户端一份声明"。
3. UA 目录对表抄录（pi-desktop/kimi-code-desktop 等）进 client.rs 指纹规则。
4. 克制：magpie 的 60+ 客户端是护城河也是包袱（其 LESSONS："A fix reaches every sibling"）——只接真实在用的。

## 12. 每请求性能指标：速度/首字/命中率（第七轮，回应"请求日志没有 tps"）

**cc-switch（路由服务直接计时，与 Zroutery 数据模型同构）**，format.ts:200-310：
- **精确速度** = `output ÷ ((latency − firstToken)/1000)`；双门槛防假速度：输出 ≥100 token **且** 生成窗口 ≥100ms——"中转站缓冲后一次性吐出、或短回复整段落在同一个网络包里，算出来的是传输突发而不是生成速度"（:214-216）
- 非流式无首字 → 无速度（不是 0）
- **会话日志导入源（无首字计时）→ 独立估算口径**：`output ÷ latency`（含首字等待），门槛更高（≥200 token 且 ≥1s），显示带 "≈"——"输出越少首字占比越大、算出来越偏低"（:266-268）
- **命中率 = cacheRead/(fresh+cacheWrite+cacheRead)**，仅真全命中才显示 100%（不把缺数据当满分）
- **汇总速度 = Σ输出 ÷ Σ生成时间，不是逐条平均**——"逐条平均会被短请求带歪"（:296-299）

**magpie（会话日志源为主，计时粗糙）**，page.js:1904-1970：
- 行速度 = 解码输出 ÷ 流式写入窗口；会话文件无首字 → **下限口径**：整轮时间算出的 floor 显示 **"≥N tok/s"**（诚实标注下限，实测比真值低 2-5%——他们拿真流校准过）
- speed 是 Overview 可切换的 tile 指标之一；口径解释进悬浮卡（"reasoning 只在可见流式时计入；隐藏 reasoning 没有速度"）

**Zroutery 转化规格（U16，数据同构 cc-switch 路由路径）**：
- `api.ts` 新增 `tps(record)`：`stream && ttft_ms != null && latency_ms − ttft_ms ≥ 100 && output_tokens ≥ 100` → `output/((latency−ttft)/1000)`，否则 null
- 流水行 meta 加两格：首字（`ms(ttft_ms)`，仅流式）与速度（`N tok/s`，仅 eligible）——自带单位不依赖表头；失败/非流式两格不渲染
- 详情抽屉补命中率（`cache_read/input`，input 为 cache-包含总数）与速度同源
- 复用约束：ttft/usage 字段已存在（E-181 论证过），纯展示层

## 13. 系统强调色的平台矩阵（第八轮：macOS/Linux 能支持吗）

**用户裁决（"md3不md3 fluent不fluent"）**：token 换色不是皮肤——**组件形态才是身份**。MD3/Fluent 的层3 覆写按组件矩阵实现（药丸按钮、tonal 表面、M3 segmented 胶囊、acrylic 侧栏、Fluent 双环焦点、infobar toast），"最少覆写"约束解除；`--control-h` 与布局桩有效性守恒；industrial 零改动。教训：**给换肤任务设"最小覆写"约束，产出必然是换色**——皮肤的工作量本来就在组件层，别在 brief 里自绑手脚。

**CSS 通道（`AccentColor` 系统颜色）的平台真相**：
- **Windows / WebView2（Chromium）✓**：映射 DWM 强调色——唯一可靠的平台
- **macOS / WKWebView ✗**：**WebKit 故意伪装**——反指纹策略返回固定默认蓝而非真实 accent（WebKit issue #136；Stack Overflow "How can I use the macOS system accent color in CSS" 确认表单控件原生反映 accent，但 CSS 关键字拿不到）
- **Linux / WebKitGTK ✗**：无统一 accent 来源（GNOME 47+ 才引入 accent-color；KDE 另有方案），映射不可靠
- 结论：纯 CSS 通道只在 Windows 成立——macOS/Linux 必须后端注入

**magpie 已实证后端注入路径**（omarchy.go:241-293）：从平台配置读 accent → 后端算出三件套 `--accent / --accent-soft（混 14% 背景）/ --accent-fg（背景色反推）` 注入 CSS 变量；TUI 无来源时用硬编码 indigo 回退。三件套正好对应我们的 primary/hover/on-primary。

**Zroutery 规格（新增 [B7]）**：Rust per-platform 读原生 accent，注入 `--system-accent` 系变量：
- Windows：DWM `ColorizationColor`（registry，ABGR 解码）或 WinRT UISettings
- macOS：`NSColor.controlAccentColor`（objc2）
- Linux：尽力而为——GNOME 47+ gsettings accent-color、KDE kdeglobals、其余回退硬编码
- 前端：`--primary: var(--system-accent, #005FB8)`；on-primary 由后端按 YIQ 亮度算好注入（用户自选色无法预设对比度）；hover/soft 同后端给
- 语义色（ok/warn/danger）不跟随系统

## 10. 客户端识别机制对比（第五轮，回应"codex_desktop 为什么未知"）

**cc-switch：身份在接入时确定，不走识别。** `proxy/server.rs:299 build_router()` 为每个应用铺**专属路径前缀**：`/claude/v1/messages`、**`/claude-desktop/v1/messages`**（连 Claude Desktop 都单列）、`/codex/v1/responses`、`/codex/v1/alpha/search`、`/gemini/…`。`AppType` 由请求路径决定（forwarder 全程携带 `&AppType`），UA 一个字都不用猜。这成立的根基是他们的接入方式：**改写每个应用的配置指向自己的代理**——写入什么路径，应用就打什么路径，身份是注入的。代价：每个新应用要加路由+改它的配置；第三方直接指向上游的流量没有归属。

**Zroutery：单端点服务所有客户端，只能从请求头归因。** client.rs 的 UA 前缀规则表（claude-cli/、codex/、opencode/…）在零配置上更通用，代价是新客户端形态漏归因——Codex Desktop 的下划线 UA 就这么漏的，再被 custom 桶 64 字符截断成 "unknow" 尾巴。

**两条结论**：
1. 近期修复不变：client.rs 规则表加 `codex_desktop` 前缀（family codex，显示名 "Codex Desktop"）——[B] 已列，GPT 6.1 SOL 一行规则+一个测试。
2. 架构级远期想法：**Zroutery 的 takeover 也是改写客户端配置的**——接入那一刻身份已知，可以注入（per-client 凭据前缀、专属 env 标记、或 codex config 里的注释字段），网关侧按注入标记归因，UA 只作未接管流量的兜底。这会把客户端归因从"识别"升级为"声明"，UA 漂移彻底免疫。改动跨协议层，先记录不排期。

## 9. 第四轮补充（2026-10-31 用户截图对照）

- **facet 筛选计数是纯前端 Map**：UsageDashboard.tsx:186-194（按模型）、473-477（按供应商）`counts.set(...)` 后排序喂级联菜单——零后端。Zroutery 落点：U14 请求流水供应商/模型 facet（账本行点击保持"在context里钻取"，facet 负责"直接跳"）。
- **用量数据来源即设置**（用量设置抽屉）：cc-switch 扫描 11 个应用的会话日志（自动扫描开关+间隔+立即同步+重建维护带备份警告）。Zroutery 的 provenance 是网关自身（始终开启、已持久化），无对应面——最近似的形态是 U1.5 CCSwitch 历史导入。不抄抽屉本身。
- **导出为文件**：cc-switch 不露原始 JSON；我们导出=textarea+复制（诚实但糙），升级为保存文件需 Tauri 对话框（[B] 小项 U15'）。
