# Zroutery 与 CC Switch / Magpie 的日常任务体验深度复核

> 日期：2026-11-06。性质：研究报告与下一轮实施建议，**不是 UI 实施、总计划变更或交付验收**。
> 研究基于当前 dirty 源码、参考源码、隔离浏览器中的真实组件与假 IPC/API。主会话仍在修改工作区，不能只用 HEAD 代表本报告看到的实现。
> 最后集中源码核对：**2026-11-06 20:23:49 +08:00 / 12:23:49 UTC**。行为截图分别记录自己的 UTC 时间。后续实施前应按符号重新核对，不机械套用行号。
> 收尾对账：20:37 +08:00，所记录的 24 个代码 / 合同文件 hash 未变，`upgrade-execution.md` 已由主会话追加到 928 行；已读其新增正确性与验证记录，本报告不借这些通过记录升级 UX 结论。

## 1. 结论：主要不是皮肤不好看，而是任务没有连续完成的地方

用户反馈“UI 体验很差，没有两个参考项目好”，有可复现依据。当前 Zroutery 已有相当多的能力与工程防护，但仍像**后端配置对象的浏览器**，没有充分变成**日常任务的操作台**。

根因按影响排列：

1. **后端所有权被误用为用户界面的分区依据。** 账户声明、凭据、余额、签到、模型目录各有合理的后端所有者；用户修复同一个账户却必须在“连接 / 账户观测 / 模型 / 极客模式”之间拼接。`Accounts.tsx` 的模块说明甚至明确把 declaration / observation 拆页作为设计理由，与接受计划要求“同一实体的配置与状态相邻”相反。
2. **移走常驻表单，却没有给动作一个可见的落点。** 页头“添加模型”展开页尾表单，在 600 模型夹具、820×560 下表单位于 `y=4292px`，焦点留在“收起”按钮。DOM 中存在表单，不等于用户点完能操作它。
3. **界面把下一步写成说明，而不是可继续的状态。** 概览已有三个步骤入口，但不判断当前缺失项；服务商创建后只有模型数，没有预选此服务商的目录入口；客户端应用后关闭抽屉，没有持续的“待重开 / 菜单待确认 / 所选档位待观察”工作区。
4. **状态与金额在最常读的位置过度简化。** 绿点常表示启用或有凭据，不表示已可调用；“已保存”可能由启动读快照或余额刷新产生；请求行费用读末次尝试，而总账读全部尝试；未知与不完整的说明常藏在 hover 或更下方的小节。
5. **视觉强调分配和信息密度没有围绕任务。** 18px 的累计说明、32px 的大区间距、重复细节分隔、60px 的价格文字列共同消耗首屏。MD3 / Fluent 已改变颜色、字体和形状，但没有改变上述结构。三皮肤×亮暗的模型页均只完整显示约 5 行。
6. **现有门禁主要证明局部正确，缺少“任务完成”的断言。** 控件对齐、页头入口存在、分页 DOM 有界、无页面级横溢出、覆盖层栈顶等测试有价值；它们不能证明添加动作后焦点到表单、失败保留草稿、从一个故障到正确账户或导出与筛选同口径。

**建议方向：保留接受的六入口、四档用途和后端边界；把基本配置、观测与修复聚合到同一实体上下文，把结果与下一步做成可恢复的任务状态。不要再以“模仿卡片 / 加图标 / 加圆角”作为主批次。**

### 1.1 本轮已经修好的东西，不应重新派工

核对了 `docs/development/upgrade-execution.md:789–797` 与实际源码，以下不作为未修缺陷：

- 简易 / 极客模式均已恢复六个任务入口；不是仍只有三个入口。
- Overview 已有“服务商 → 模型与路由 → 客户端接入”路径与中文四档用途，且不保证档位天然便宜、快速或最强。
- 外部历史导入已有选择来源、预览、结果阶段，范围 / 副本名、取消、保存预览重开；后端确认发布的是已暂存快照，不再确认时重读源库。
- RequestDrawer 与历史导入 Drawer 已 `showSaveStatus={false}`；不是所有只读抽屉仍显示配置保存成功。
- 尝试详情已区分“客户端 → Zroutery”和“Zroutery → 上游”，并有结构化型号声明、未知 / 冲突提示。研究后段新增的 `usage_evidence` 显示亦已复核，不再列为缺失。
- 公共 class effort 已按精确服务商 / 模型 / API Surface 同源取交集，人工空集合不会被目录刷新替换。
- NewAPI 公开报价已改为**账户 / Key 作用域、明确估算的兜底**，不是继续覆盖整个服务商的所有模型价格。

这些修复确实改善了基础路径；本报告讨论的是**修复以后仍不顺手的原因**。

## 2. 研究方法、基线与边界

### 2.1 源码基线

| 对象 | 本轮核对版本 | 使用方式 |
| --- | --- | --- |
| Zroutery | `dev`；HEAD 与本地 `origin/dev` 均为 `4075a54f949e4448976965c25a6e08abad7f0715`；大量已有 dirty，index 空 | 读取当前工作区，未改 UI / Rust / 源计划 / 共享证据；不是只审已提交版本 |
| CC Switch | `ba3d80d87ce7e3fe1ed52a5d5cbe9fa79eb5898e`，参考目录 clean | `E:\zr-research\cc-switch` 只读；组件 / CSS 复制到自有临时目录，替换 Tauri 边界 |
| Magpie | `a6951cc63af6b992625a96d93aafe438c14ffab5`，参考目录 clean | `E:\zr-research\magpie` 只读；直接服务原始静态资源，所有 `/api/` 回答来自自建假数据 |

已完整阅读唯一接受计划 544 行，重点为第 7.1–7.5 节与五条任务；同时核对 `AGENTS.md`、`CONTRIBUTING.md`、`WORKFLOW.md`、`upgrade-contracts.md` 和 2026-11-06 的执行增量。

本报告新建文件是本轮唯一仓库写入。没有提交、暂存、运行配置更换、部署、真实推理、账户探测、签到、登录或客户端文件写入。

### 2.2 行为观察怎样进行

- Zroutery 使用既有 `http://127.0.0.1:5186/mock.html`；600 模型使用 `?stress=600`。不使用或停止既有 `localhost:5173/preview.html`、桌面应用和网关。
- 独立 Chrome headless profile，通过 CDP 观察 DOM、实绘截图、边界框与焦点；主要窗口为 **820×560、1000×720，DPR=1**。点击 / Enter / Escape 的关键负例使用真实 Input 域事件；少数用于定位参考流程的调用是页面内函数 / DOM 点击，单独标注。
- 所有参考账号、Key、请求、费用与错误都是发明的 fixture；浏览器外部网络被限制。参考渲染代码不是重新画的设计稿，后端真实性不是本轮验收对象。
- CC Switch 临时依赖按原 `package.json` 范围重新解析安装，**没有复用原锁版本**；不能据此比较发行稳定性、首绘性能或包体。其 Windows 配置最小窗口为 **1000×640**（`src-tauri/tauri.windows.conf.json:14–15`）；820×560 观察仅是超出支持尺寸的压力对照，不是要求参考产品通过 Zroutery 的最小窗合同。
- 未做真实 WebView2、原生模型菜单、真实中文 IME、Windows 高 DPI / 多屏、macOS / Linux、实际账单 / 价格 / 身份验收；不从浏览器 mock 扩大结论。

### 2.3 按用户要求拆成了三个独立小活

`subagent` 调用未成功后，依用户要求改为三条独立 `zroutery/lightvela-auto` 会话，只读取证并自动回传，全部完成；未授权它们实施或写主报告：

- 保存 / 焦点：`ses_e4f702c23ffeGSCPBIe8IRoE6r`；源码核对与独立 fake Tauri 失败注入。
- 用量口径：`ses_e4f6f7398ffeMt87DaTMi4W2ha`；以事实类型、聚合 SQL、UI 消费链为主。
- 账户 → 模型 → 接入：`ses_e4f6e3ff0ffe10GcudePmsxjEr`；源码与独立 mock 观察。

协调会话重新核对关键结论与当前行号，并补做了可信按键、添加失败、多尝试费用、三皮肤亮暗、参考模型 / Key / 接入 / 用量 / 失败路径观察。**不是把子会话结论直接拼接成报告**；撤回或收窄的结论见第 8 节。

## 3. 可复现的体验证据

自有材料目录：`C:\Users\InfinityNeko\AppData\Local\Temp\opencode\ui-reference-review-20261106`。下列 `.png` 旁通常有同名 `.json`，含 UTC 时间、真实窗口、控件和区域边界；这些是本机研究材料，未复制进仓库。

| 编号 | 操作 / 观察 | 结果与材料 |
| --- | --- | --- |
| O1 | Zroutery 默认简易、小数据、820×560，概览顶部 | `.content.scrollHeight=1011`；最近请求从 `y=853` 开始。`zr-final-overview-simple-820.png/json`；20:11 +08 复测 |
| O2 | Zroutery 600 模型，点击页头“添加模型” | `scrollTop=0`；焦点仍为“收起”；表单 `y=4292`、pager `y=4245`。`zstress-final-add-invisible-820.png/json`；20:11 +08 复测 |
| O3 | Zroutery 用量、小数据、820×560 | 早轮极客 / Industrial：失败筛选约 `y=944`、首请求 `y=989`；最后默认简易：筛选 `y=1014`、首请求 `y=1059`。两者首屏都 **0 条完整请求**。`zr-activity-top-820.png/json`、`zr-final-activity-simple-820.png/json`。差异来自模式与预算说明，不能把单个坐标当所有布局的常数 |
| O4 | Zroutery 同一模型页，三皮肤×亮暗，820×560 | 六态均约 5 行完整可见；价格列均 60px、普通换行。实际字体分别 Noto Serif SC / Roboto+系统中文回退 / Segoe UI。`zr-skin-<industrial\|md3\|fluent>-<light\|dark>-models-820.png/json` 与 `skin-observations.json` |
| O5 | Zroutery 添加服务商，fake `save_config` 拒绝 | 1 次写调用后表单消失、无 Drawer；只剩全局错误，草稿不可继续。`zr-save-refused-add.png`、`zr-save-behavior-evidence.json`；20:17 +08 新实测 |
| O6 | Zroutery dirty TextField，一次可信 Enter / Escape | Enter 产生 **2 次 `save_config`**；dirty Escape 关闭整个 Drawer、0 次保存；构造 `isComposing=true` Enter 仍提交，但不是原生 IME 验收。同 O5 JSON；独立小活亦复现 CompactNumber |
| O7 | Zroutery 新开服务商 / 同步菜单 Drawer，没有在该实体保存 | 直接出现启动读取留下的“已保存”；余额刷新不调用 `save_config` 也更新时间。稳定截图 `zr-provider-drawer-simple-820.png/json`、`zr-client-sync-1000.png/json`；末轮 DOM 记录在 `final-observations.json`，末轮 provider PNG 在入场动画中，不用于评视觉。保存小活补验证刷新负例 |
| O8 | Zroutery fake 两尝试请求：失败 0.01 USD、成功 0.02 USD | 行显示 0.02；详情“用量 / 成本”显示 0.02，下方“已知费用”显示 0.03。`zr-multi-attempt-cost.png`、`zr-multi-attempt-cost-evidence.json`；20:15 +08，证明 UI 消费差异，不是实结算 |
| O9 | CC Switch 添加 DeepSeek 预设、填假 Key、提交 | 原组件完成预设 → 表单 → 列表；提交按钮在固定 footer；当前供应商行有“使用中”，页头有实际直连目标。`cc-add-picker-1000.png`、`cc-preset-form-1000.png`、`cc-add-preset-1000.png`；不证明假上游可用 |
| O10 | Magpie 原服务商编辑器、改模型 / 名称，假保存失败 | 编辑器内草稿保留；按钮“保存中…”后恢复；错误靠近 footer。`mp-provider-editor-1000.png`、`mp-save-failure-1000.png`。长表单仍有内部滚动，不是无成本体验 |
| O11 | Magpie 两 Key，其中一个假 401 暂停 | 同编辑器显示 Key 名、首选、模型范围、原因 / 恢复时刻、“更换密钥 / 立即重试”；假 API 成功后提示消失，编辑器仍开。`mp-keys-repair-1000.png`。操作位置与说明来自原组件；恢复由 fixture 驱动 |
| O12 | Magpie 假客户端接入 / 用量失败筛选 / 请求详情 → 路由 | 原客户端行说明在 `/model` 或 `/models` 选模；失败筛选在 `y=92`，筛选参数含时区与 `failed=1`；请求详情内有“查看路由”，可到失败原因 / 尝试叙事。`mp-usage-requests-top-820.png`、`mp-final-failed-detail-820.png`、`mp-route-failed-1000.png` |
| O13 | CC Switch 用量，1000×720 | 状态筛选约 `y=413.5`，首行 `y=486`；40px 行，约 5 行完整可见。宽表在自有横滚中，部分列不在首屏。`cc-usage-top-1000.png`、`cc-request-error-detail-1000.png`；失败详情仍显示已知费用 |

Magpie 的请求表在 820×560 下同样低于首屏（约 `y=929`），**不能宣称它所有页都更短或无需滚动**。它的实质优势是顶部筛选可达、按同一查询解释读数、错误就地、从记录续接路由。CC Switch 的优势不能被其更大 Windows 最小窗条件掩盖，也不能反过来用它的更大最小窗放宽 Zroutery 的 820×560 目标。

误测或 fixture 漂移不用于产品结论：早期非精确窗口截图、Magpie V1 未给 movable 元数据而露出退休订阅的添加页、增强 fixture 初版缺聚合字段导致 NaN、mock 导入后累计不增长 / 覆盖日数为 0 / 不适当协议字段。后续使用修正边界数据；不是参考或 Zroutery 后端缺陷。新挂载后仅等两帧的少数截图仍处入场动画，不能拿其淡化 / 位移当稳定外观缺陷；稳定样式对照用第 O4 的六态截图及早轮稳定视图。CC Switch 临时环境首次因缺 Prettier / Tailwind 动画依赖无法渲染，临时依赖 / 配置补齐后才进入 O9/O13；该准备失败不是参考产品故障，也未计为成功验收。

## 4. 五条接受任务的并排分析

### 4.1 添加 / 导入服务商：知道在哪登录或填 Key，同处看身份、价格、余额和原因

**参考项目怎样减少用户负担**

- CC Switch 的第一上下文是客户端。页头新增进入两步预设选择：账号登录 / 模型厂商 / 第三方 / 云 / 本地，而非先让用户识别协议；选定后保留预设身份与取 Key 链接。依据：`src/components/providers/forms/ProviderPresetSelector.tsx:142–215,774–840`，`AddProviderDialog.tsx:459–465,488–562`。
- CC Switch 没有把“已配置”和“正在使用”合成一个绿点。`mode/SwitchModePanel.tsx:287–311` 的实际连接状态独立于查看的 mode tab；`presentation.ts:53–79` 单独承载状态、按钮、禁用原因和有条件的健康信息。
- Magpie 在同一 `renderEditor` 中承载端点、Key、模型、账号与路由选项；`app.js:14577–14814::renderKeyAccounts` 把暂停原因放在 Key 自己的行，同时保留替换 / 重试；`13218–13324::renderAccounts` 明确“原因不能取代重新登录动作”。O10/O11 验证了原组件的局部保存与修复位置。

**Zroutery 当前任务在哪里断开**

1. **默认模式隐藏了基本修复，而不是只隐藏高级选项。** `Providers.tsx:1895,1899,2115,2299` 把面板、端点编辑、余额探测声明、账户声明整个置于 `geek`；简易模式保留认证信息 / 缺 Key 输入、启用与移除。已有连接的 URL 修错、添加第二个推理 Key、修面板 Token，都可能先要求用户发现模式切换。
2. Accounts 不是同一实体的完整管理面。`Accounts.tsx:1–22,223–418` 展示状态、余额、签到和浏览器取 Token，但没有手工凭据编辑 / 跳到目标账户声明。其引导 `i18n.tsx:1737` 指向在简易模式不可见的“服务商的账户设置”。浏览器捕获入口两模式都在，**不能说所有凭据路径完全不可达**；真正断点是缺少同上下文的可选修复路径与明确定位。
3. AddProviderForm 先问本地名、协议、Base URL，创建后才填 Key。`Providers.tsx:716–747` 又在未等自己的 `save` 结果时清空、回调关闭。O5 是明确的失败恢复缺陷，不是审美问题。
4. ProviderDrawer 的“模型 / 该服务商 N 个”是死数字（`:1834–1839`）。没有目录 / 模型列表 / 下一步；已有账户与模型信息也分散在别的 tab。
5. CC Switch 配置导入在页尾内嵌表格（`:520–565`）；历史导入已独立成阶段 Drawer。两种导入的任务不同，不能强行合成一种存储，但都应有可见的预览落点、真实结果和后续动作。当前配置导入成功 Toast 用**选中数**（`:355–363`）；后端可跳过已存在 / 无效条目，详细报告主要在日志（`commands.rs:1688–1827`），因此选中数不是实际新增数。

**根因与目标行为**

不要把全部高级字段放上首屏；应让**服务商 → 账户 → 该账户 Key**成为同一个可展开管理上下文。后端仍分别拥有 config / binding / quota / rates，UI 用稳定身份关联它们，不持久化运行观测到配置。

- 现状：缺凭据提示 → 用户找“账户设置” → 服务商 Drawer 简易无此区 → 切极客 → 找同一账户 → 设置 → 再去观测 / 模型页。
- 建议：缺凭据行 → “更新面板令牌 / 用浏览器获取 / 查看身份” → 暂存 / 验证 / 必要时确认换身份 → 发布结果 → 保留余额 / 价格 / 模型续接。全程不因基本任务切模式。
- 添加成功后给“凭据已存 / 未存”“从此服务商获取模型”下一步， provider 已预选；失败留草稿，竞态重复不打开同名旧实体。

**最小受影响范围**：`Providers.tsx`、`Accounts.tsx`、`AccountBindingReview.tsx`、`BrowserTokenButton.tsx`、`App.tsx` 的实体定位；三语词典。真实配置导入结果需要 `api.ts` → Rust 命令 → `mock-bridge.ts` 的小合同同步，不应扩大为重做历史导入。

**可执行验收**：默认简易模式在指定账户行修复缺凭据，不依赖模式切换；确认不同远端身份时保留旧身份账目且不影响兄弟账户；写失败草稿仍在；“获取模型”预选原 provider；导入选中 3 / 实际新增 1 / 已存在 1 / 跳过 1 时明确报告四种数字而不说“导入 3 成功”。

### 4.2 分档选模：按服务商找可读模型名，确认建议，看到规则来源与最终候选

**参考优势及适用边界**

- Magpie 从服务商编辑器直接勾选它发布给客户端的模型；“仅所选 / 每系最新 / 仅看用量”和“Agent 看到的是已勾选模型”相邻，用户不用先理解全局 registry。CC Switch 的 `forms/shared/ModelInputWithFetch.tsx:31–77` 把获取目录 / 可搜索模型选择放在要填写模型的位置。
- 两者不等于 Zroutery 的四档契约。CC Switch 的客户端角色映射与 Magpie 的任意组 / 订阅策略不能代替“一模型一个主档、建议经确认、共同 effort 交集”。借鉴任务位置与解释方式，不移植业务假设。

**Zroutery 当前事实**

1. `Models.tsx:108–118` 已按 provider 名和可读模型名排序，`:285–321` 支持显示名、上游 ID、别名搜索；这是有效改善。但 `:562–566` 仍是无分组标题的全局平表，provider 重复在每行副标题，没有 provider 导航 / 筛选 / 组内操作。标题“按服务商查看模型”比实际组织方式强。
2. O2 的表单落点约 4,292px，是 `adding` 在页头切换、`<Section>` 在 pager 之后挂载（`:508–510,578–683`）的直接后果；不需要虚拟化才能修。
3. 用途选择仍不统一。首页和模型行副标题已中文四档；ModelDrawer `:848–860`、目录批次 `:1261–1268`、Routing `:230,881,1026,1060` 仍以 `fast-class` 等技术入口作选项 / 主标签。选择器不应迫使用户把“推理用途”和 `high / max` 参数当一回事。
4. CatalogueImport 是“勾选 + 为整批设置一个档”，没有理由 / 来源 / 冲突逐行确认（`:1142,1194–1225`）。**默认不分档**是比猜标准更诚实的行为，应保留；不足在于缺少被接受的建议与确认流程。
5. Routing 的四档池来自 `tierMembers`（`api.ts:2894–2902`：启用过滤 + priority 排序），不是针对某次请求的实时可执行候选。页面对非 Priority 有诚实脚注（`Routing.tsx:253–265`），但没有费用 / 能力 / 钱包 / 账户排除原因和具体请求选路续接；不能给它换个“实际候选”标题就当完成。
6. 默认编辑入口是全局 Drawer（`Routing.tsx:844–986`）；未见普通用户可操作的逐档覆盖 / 恢复继承。Core 已有 `config.routing.policies` 与客户端 profile / policy 决策，不应重造所有策略；目前 UI 合同和显示没有接出被接受的“共享默认 → 本档覆盖 → 命中高级规则”。
7. 模型页消费 `reasoning_capability` 以决定当前集合（`Models.tsx:819–823`），人工编辑会写 source / observed_at / surface（`:1069–1079`），但没有就地展示其来源 / 时间 / 人工修正。模型 `pricing` 行也不是该账户 / Key 的已验证有效费率，尤其 NewAPI 兜底现在存于账户作用域。

**建议的连续流程**

服务商分组 / 筛选 → 组内“获取模型” → 可读名为主，ID 可搜索 → 每行显示建议档 / 理由 / 来源 / 冲突，未知保留待确认 → 批量确认 → 四档摘要显示继承来源和当前值 → 具体请求证据查看真正候选 / 排除原因。

先做能独立改善的 UI：添加用任务面承载并聚焦、provider 预选、分组 / provider filter、中文档位、格式化价格。再接**有类型的分档建议与有效规则投影**，不要提前画出后端不具备的“实际可用”状态。

**最小范围**：`Models.tsx`、`Routing.tsx`、`Overview.tsx`、`App.tsx`；建议 / 覆盖 / 有效候选投影需 `api.ts` 与 Core config / policy / router 配合。保留已有稳定 ID、人工选择、报价 / 身份绑定，不通过改显示名消除身份问题。

**验收**：600 模型下不用滚完整个当前页即可新增 / 翻页；返回仍在原 provider / 搜索 / 页码；同名不同 provider 清楚区分；人工档位不被重新导入覆盖；未知建议不会自动成为标准；改显示名不改上游值；改变本档规则只影响本档，能恢复继承，显示值与执行 trace 对齐。

### 4.3 接入客户端：一键同步、原生选择四档、观测正确池、恢复原配置

**参考的强项是说清“正在看”和“实际生效”**

- CC Switch mode tab 只负责查看，必须显式按钮才能生效（`SwitchModePanel.tsx:97–100,287–311`）；服务停止、启动失败、旧目录都有就地 CTA（`:376–504`），stale 提示按 revision 重现（`:158–163`）。
- Magpie `connectSwitch` / `connectLine`（`app.js:1177–1213,1370–1459`）让客户端行回答一件事：是否接入、在哪里选模型、是否待重开、怎么修漂移。它区分终端 / IDE / app / daemon 的重开说明（`:1339–1367`），不会把一条“请重启”当所有入口的同义词。O12 的 fake 接入结果证明原组件给出后续选择位置，不证明原生菜单已加载。

**Zroutery 的闭环缺口**

1. ready 行同时有“同步模型菜单”和“接管”（`Takeover.tsx:423–433`）。当前后端旧 adopt 最终也调用 `client_setup_apply`、使用保存的 options（`state/takeover.rs:492`）；不能把它误称为仅写 URL 的另一套产品。**两条相邻入口暴露了实施历史，用户仍不知道哪一个是完整四档接入。**
2. `openSetup` 每次把 `default_class` 设为 null / standby=false（`:84–90`）；status 与 preview 不回传当前默认档（`api.ts:1378–1396,1637–1654`）。后端实际上保存 `takeover/<client>-selection.json`，由 `setup_options` / `target_for` 读取（`state/takeover.rs:260–283,569`），不是持久化整个 `GatewayTarget`。用户重开只看见“保留”，不知道保留的是什么。
3. 同步应用后 `closeSetup()`（`:111–123`）；Codex 重开要求在英文 notes / 说明中，而不是独立可持续验证步骤。not_detected 有路径与自动轮询（`:155–168,477–486`），但没有就地重新扫描 / 可执行引导；available / error 没有修复续接。
4. `KnownBlocker` / `KnownReason` 用英文正则猜原因（`:556–568`）；新后端原因落回英文散文。应返回原因码 / 参数，而不是让翻译函数猜业务状态。
5. 验证已经比“写成功=接管成功”诚实：Core 要 config_matches、credential_ok、request_confirmed 才 Active（`ownership.rs:1349–1376`）。但请求观察只有客户端级时间 / boolean：`Stats::observe_client`、`client_seen_since`（`stats.rs:605–627`）与桌面 `verification_from_config`（`state/takeover.rs:684–694`），**没有所选 class、实际入口 / 版本和最近对应请求**。已认证网关请求的客户端归因亦不等于强客户端身份认证，源码对此已有说明。
6. normal mock 无 `client_setup_preview/apply`、Token capture fixture，`get_usage_attempts` 返回空（`ui/mock-bridge.ts:1273`）。O7 的预览失败是研究覆盖缺口，不是生产命令缺失。

**目标流程**

客户端行“配置四档接入” → 默认档（回显当前值，保留=不变更） → 四槽可用 / 受限、影响文件预览 → 确认应用 → 留在该客户端的验证面 → 对应客户端 / 入口的重开说明 → 用户自己的请求证据 → 所选 class / 实际候选池 / 恢复入口。

用一条主要任务入口包装现有安全机制；保留释放 / 冲突解决作为次动作。不要合并掉真实 state，也不要自动启动客户端、发送付费“验证请求”或替用户恢复文件。

**合同与范围**：`Takeover.tsx`、`api.ts`、`mock-bridge.ts`；Rust `state/takeover.rs` 与 Core `agent_takeover/status.rs` 的当前选择 / 原因码 / 证据视图；按 class 观察需 Stats / ingress 真实支持。结构字段由主会话协调，不是研究会话直接改。

**验收**：默认“推理”重开可见 / 可保留；预览后外部编辑触发 stale 拒绝并能重新预览；写成功但未观察目标 class 仍明确“待验证”；仅观察快速类不能宣称所选推理类已确认；三客户端分别用真实菜单 / 请求验证，不互借通过；恢复仅管理字段，用户无关编辑仍在。

### 4.4 定位失败：从概览 / 请求直接到正确账户、Key、能力、价格或兼容问题

**参考借鉴点**

- CC Switch 记录表点击进入请求详情，来源 / 状态码 / 成本明细清楚；O13 的失败详情保留已知费用。它也有纯图标动作和长 Drawer，不能假定可发现性处处理想。
- Magpie `plugins/usage/page.js:1197–1247,2259–2264` 让详情与时间链接到该请求路由；`plugins/routing/client.js:3019–3037,698–808` 展示的是当时 trace 的顺序 / 原因，而不是重算当前配置。暂停 Key 的修复仍在同一上下文（`:4034–4054`）。O12 复现假 400 → 详情 → 路由 / 原话；没有模拟新的上游调用。

**Zroutery 的具体断点**

1. O1 / O3：日常故障入口低于首屏；Overview 最近请求还都是不可点击的 `span`（`Overview.tsx:379–404`），只有“全部活动”跳整页。provider 行也只跳整个 providers 页（`:315–325`），不能定位刚才那个故障实体。
2. provider / model 绿点并非可调用证明：Providers `:462–468` 是启用 / 凭据存在；Overview 所有启用 provider 都 `tone=ok`（`:315–316`）；Models `:277–282` 用关闭 / 冷却区别，但 screen-reader 状态只说启用 / 关闭（`:758–765`）。视觉 / 可访问状态应一致说真实证据，不拿“没有近期故障”当已验证。
3. RequestDrawer 已有尝试列表、声明与用量依据，但未把 `execution.account_id / inference_key_id / binding_version / pricing` 呈现为每尝试的定位 / 价格上下文（`Activity.tsx:2081–2100`、`api.ts:686–711`）。没有“处理此账户 / 查看该模型能力 / 价格依据”的动作。
4. 详情里的“实际响应”读 `resolved_model`（`:2049–2056`），它是路由解析身份；具体上游型号声明另在 attempt 小节。措辞容易重新把“选到哪个配置”与“上游声明谁回答”混淆。已加声明免责声明应保留，并把两个名字在主摘要也分开。
5. 费用主读数会不一致（O8）：后端保存全部已知失败尝试成本是正确的，行与“用量 / 成本”却读 final-attempt cost。失败行还只显示状态，不显示已知费用（`:1961–1965`）。这直接影响对“这次失败花了多少”的判断。

**目标流程**

概览待处理问题 / 最近失败 → 指定 request（稳定 ID） → 一眼读到请求入口 / 解析池 / 最终结果 / 已知总费与未知部分 → 每尝试 identity / protocol / declaration / reason → 指定当前实体的管理位置或历史快照。

历史实体已删除 / 绑定已换代时，用记录中的名称和版本解释，提供“当前已变更 / 无对应配置”，不能跳到同名新账户假装它是当时身份。只读诊断不自动解除闩锁、不重放请求 / 工具。

**范围**：`Overview.tsx`、`Activity.tsx`、`App.tsx` 的类型化实体 / 请求 locator；provider / model / account 定位；必要时 `api.ts` 与诊断投影。从既有 attempt facts 做简单表与原因摘要，**不必移植 Magpie 的动画鸟或完整图形舞台**。

**验收**：首页失败可直达指定请求，不丢原 filter / 页码 / 滚动；两次尝试的身份、协议和费用分别可追溯；已知 0.01 + 0.02 显示 0.03，含未知时仍保留已知部分；原配置已删除不伪造当前归属；缺诊断明确未知，不用日志正文猜真实型号。

### 4.5 查询 / 导入用量：明细、汇总、来源、未知与重复可解释，图表 / 列表 / 导出同口径

**参考强项与限制**

- CC Switch 的 range / appType / provider / model 穿过 summary / trend / logs / provider / model 表，query key 也含这些字段（`UsageDashboard.tsx:716–736,848–878`、`lib/query/usage.ts:43–166`）。**状态码是日志筛选，不进入所有 summary**，不能夸大为它每种筛选都已全贯通。
- Magpie `ledParams`（`plugins/usage/page.js:1035–1053`）让请求、对应聚合 / 趋势、CSV 共用参数；`ledRead` 取消旧筛选读、保留当前表等待新数据（`:1055–1121`）。筛选为同一查询，并不要求把概览 / 年热力图也偷偷算作同一窗口。
- Magpie 年热力图是明确的另一面，主动删掉 period / day（`:1646–1665`），不是通过“柱状图 → 热力图”偷偷改窗口；它不能作为放宽 Zroutery 三样式同窗口合同的借口。

**当前用量页并不是一个查询**

| 面 | 当前数据 / 窗口 | 问题 |
| --- | --- | --- |
| KPI | `stats.summary` + 全库 imported totals；`commands.rs:149–157` | 所有历史，不随列表 provider / client / failure 筛选 |
| 三样式图 | 一次读 371 天；heatmap 全数组，bars / line 默认末 30 天；`UsageCharts.tsx:116–117,191–194` | 切样式改变时间窗口；费用日列已存在但 UI 未绘 |
| 账本切法 | 本机全历史 per_client / per_model，导入缺归因处已有说明 | 说明是正确的边界，不等于已经支持统一来源 / 明细查询 |
| 请求流 | production `stats.recent(200)` → JS 筛选 → 每页 50；`commands.rs:169`、`Activity.tsx:845–860,953–956` | 只能查 recent 窗口；旧故障可能有累计却找不到；facet 仅来自此窗口 |
| 导入提示数 | `get_imported_usage(days)` 依 371 天限制，起点另读全库；`commands.rs:461–469` | 提示数与上方全库累计窗口不一致，旧导入可能存在却提示没有 |
| 导出 | 全部本地 `usage` 归档；无当前过滤参数，不含独立 imported facts；`commands.rs:1464–1505`、`stats/history.rs:110–144` | 不是“导出当前查询”；便携备份和查询导出需各自命名 |

后端 **已有** `query_usage_history`，UI 桥 `api.usageHistory` 当前无页面调用（`api.ts:2044–2045`）。它是 **OFFSET + expected_revision** 快照一致分页（`runtime_store/history.rs:32–114`、`stats/history.rs:1–2`），不是已实现 keyset cursor。先接出已有桥即可让本地 >200 条记录可查询；统一导入事实 / 来源 / 图表 / 导出才需要进一步合同，不能把后者塞成一个“小改 API 调用”。

**更深的数据表达缺口**

1. **成功率混合了不同可观察总体。** ImportedFact 有 `ok: Option<bool>`；Claude 行 `ok=None / failures=0`，OpenCode 非显式失败亦 None；CC Switch rollup 则虽 `ok=None`，却有已知 success / failure 计数（`history_import/claudecode.rs:411–416`、`opencode.rs:295–331`、`ccswitch.rs:494–547`）。聚合只 SUM(requests, failures)（`runtime_store/imports.rs:417–443`），UI 用 `(requests-failures)/requests` 并标“成功”（`Activity.tsx:663–666,1576–1587`）。它无法区分状态未知和只保留完成回复、缺失败历史的来源。**不能靠 `ok IS NULL` 一条规则修复：会把有可信成功数的 rollup 也误判未知。**
2. **未知费用的可见性不够。** known_cost / unknown_cost_attempts 已在数据，KPI 未计价说明却主要在 title（`:1615–1622`），行又隐藏未计价。读者不 hover 可能把已知小计当完整总费；应给常驻“已知费用 + N 次全部 / 部分未计价”，不压成未分币种一数。
3. **重复 / 汇总的处理入口没有闭环。** 预览有疑似重复计数，但无逐条映射 / 确认工具；发布结果后不能按该来源 / 批次进入已导入明细，汇总也没有独立浏览方式。已确认疑似不进准确 totals 应保留；不能为凑可展开记录伪造 RequestRecord 状态、时延、尝试。
4. **普通回切页丢查询上下文。** App 条件卸载 Activity（`:858`）和 tab panel（`components.tsx:494–552`）使范围 / 样式 / 指标 / 切法 / provider / model / client / failedOnly / 页码 / openId / source 等局部 state 重置。保存预览由后端恢复是另一层，不能据此说所有视图状态已恢复。
5. **查询失败 / 未覆盖 / 真零需要显式区分。** fallback `usageDays` 失败会写空数组（`Activity.tsx:318–323`）；当前 UI 有 sourceError，因此不能说所有错误都完全静默，但数值面仍不能由 `[]` 或全零独自证明安静期。源缺缓存字段的裁定见第 8 节，不能直接批量改未知。

**目标流程与合同**

顶部一个明确的查询：时间（本地日边界转换）、来源、客户端、服务商 / 模型、完整性与结果；请求与对应图表 / 小计 / 查询导出共用它。便携备份继续独立。年历若保留全年独立语义，应作为明确的“年度概览”，并先协调接受计划的同窗口约束，不静默改合同。

导入 source → preview → publish 的现有原子机制保留；增加结果到来源 / 批次查询的续接、明细 / 汇总不同形状、未知 / 疑似 / 覆盖解释。真游标 / 有界扫描恢复、Codex 正例等仍属原 M2，不以这轮 UI 报告宣称完成。

**验收**：本地 500 条可到第 5 页及更后，不因 recent 截断误报无历史；翻页 revision 变化有明确重新开始；选某来源 / provider / 时段后图、表、已知费与导出可对账；成功 / 失败 / 状态未知及失败覆盖范围可读；包含 rollup 已知计数的负例不被全部归未知；疑似重复不进准确总费；取消 / 重复导入 / 迟到更新不改预算。

## 5. 保存、焦点、恢复：应当先修的横向合同

### 5.1 “动作完成”必须等自己的结果，不等于启动了 Promise

`Providers.tsx::addProvider` 未 await，与 `Models.tsx::add` 已有的 `ok && committed` 判定形成鲜明对照。O5 / O6 在最后源码状态仍复现：

- 添加失败：表单关闭、无目标 Drawer、草稿丢失。
- TextField `components.tsx:1339–1347`：Enter `commit()` 后 `blur()`，blur 又 `commit()`；新 prop 未回写前两个调用都认为 dirty。CompactNumber / CommitInput / CommitNumber 同型（`:1447–1455,1497–1505,1558–1566`）。
- Drawer 在 window capture 阶段先处理 Escape（`:984–1021`），抢在输入自己的还原逻辑前关闭整层；不是“输入 Esc 先还原，然后再 Esc 关层”。
- 无 IME guard 的构造事件证据成立，真实 IME 必须另测；不能把构造 KeyboardEvent 当原生中文合成测试。

CC Switch `FullScreenPanel.tsx:116–138` 给文本输入 / defaultPrevented / 子覆盖层优先；`ime-safe-input.tsx:28–94` 把合成区与提交基线分离。Magpie `cancelEdit` 脏编辑确认（`app.js:7806–7819`）、保留输入 DOM 的 `replaceKeeping`（`:243–255`）是有针对性的行为范式，不是“加 React 库”需求。

建议提交去重以一次意图 / in-flight 值为单位，保存失败仍允许原稿重试；不能仅把 optimistic baseline 设成新值导致失败后“已不 dirty”。IME 上屏不提交 / 不 blur；文本 Esc 消费后才允许外层关闭；有未提交草稿时关闭 / 导航确认或保留明确的内存草稿。**秘密草稿不得写 localStorage、日志、fixture 输出或报告。**

### 5.2 保存反馈不能从任意快照推断

`saveQueue.ts:11,19–35` 的任务返回 Snapshot；`App.tsx:222–257` 对任意 `run` 都写 saving → saved，启动就 `run(api.snapshot)`。Providers 余额刷新、Accounts 探测后 snapshot、BrowserTokenButton 捕获后 snapshot 都沿此路径。O7 是这个结果，不是异常时间格式。

仍默认显示的 Drawer：Provider、Model、两种 Routing、Takeover 同步、AccountBindingReview。Request / import 已排除，不能重报。

建议把**配置写 / 身份发布 / 运行观察 / 读取**作为操作类型；局部反馈绑定 operation ID、实体 scope 与对应 revision。“已保存”只回答本次写；“余额已刷新 / 更新失败，仍显示上次有效值”回答观察。新开另一实体不继承旧成功。

保留串行写队列、失败丢弃后续待写通知、configEpoch / revision / 绑定 CAS。**不能为了修文案，简单把凭据变更绕开写队列或让较旧观察覆盖刚保存配置。** 纯读走已有有版本保护的 refresh，或在队列结果元数据中抑制错误的保存宣称。

### 5.3 保留上下文，但不要长期保留过时的调查条件

至少会话内保留 provider / query / page / scroll 和明确手选的 filters。跨页诊断借来的 filter，应可一键返回原调查，再按生命周期退出。Magpie `ledOwn / ledLent / ledLeave`（`plugins/usage/page.js:975–1033`）提供“手选条件”和“临时下钻条件”分开的参考；CC Switch 只持久化一部分偏好（`rememberedChoice.ts:7–14`），其 app / provider / model 并非全部永久记忆。

任务导航建议使用类型化 locator，例如 page + provider_id / account_id / model_id / request_id + 子区；只保存定位元数据，不复制凭据 / 正文。实体已不存在时显示解释与返回，不打开同名对象代替。

## 6. 视觉与密度：为何当前看起来“既简陋又费劲”

### 6.1 不是要求放弃 Industrial 或统一改成卡片

`skin-spec-md3-fluent.md` 已接受 Industrial=Noto Serif SC、MD3=Roboto、Fluent=Segoe；控制高度 30px 是共同约束。本报告没有授权改默认字体 / 默认皮肤，也不把用户不喜欢默认当“未经确认可以替换品牌”。

但实绘能解释当前感觉：

- 页面标题在 38px mainbar 内只 12px（`styles.css:1130–1152`）；“8 个模型，点击编辑…”这类累计说明却 18px/600（`:1214–1219`）。说明文字比真正任务标题强，主“添加模型”反而是弱文字动作。
- Industrial `--line=#000`，`.list-row` 用 `--border-primary`（`:63,83,1396–1402`），同背景页面 / 侧栏配强黑分隔线，容易读成一张缺少层级的表格。零圆角并不必然要求每条装饰线和控件边界同等强。
- 表单 / 数据 / 描述共用大 `page gap=32`、`drawer gap=24`、Section 标题 18px（`:1180,1308,1590`）。每个局部都“整齐”，叠起来却先看数屏说明才能处理请求。
- 模型 price 是“输入 X / 输出 Y · USD/百万”整句（`Models.tsx:302–308`），放进 820px 断点固定 60px 列（`styles.css:4083–4086`），数字 / 单位分三四行。600 夹具还用 `String` 输出 `1.4700000000000002` 等浮点尾数；扩大 window 后仍可能出现长值，格式化是数据展示修复。
- 未计价 / 未测行选择空白，是减少破折号的合理动机；但全局空白无法区分“没配置价格”“未知”“不适用”，需要一个可见的状态标签或列头口径，而不是几十个 dash 或全部无声。
- 1000px 时所有复杂编辑仍通用 420px Drawer，820px 时全窗口（`:1536,4066`）。大窗口复杂 provider / pricing / client preview 不应该仅因共享 Drawer 而固定窄列；具体宽度和全页编辑是下一轮可选方案，需原型确认。

### 6.2 推荐的结构调整，而非涂色

1. **一个明显主任务，若干明确次动作。** 页头“添加”用现有主按钮语义；刷新、导出、复制仍次动作。按钮尺寸、焦点与状态跨皮肤同义，不用 hover 才能判断它可点。
2. **数据行按字段布局，不把解释压进数字列。** 窄窗价格放在模型副行或简短“输入 / 输出”两数，单位在列头 / 一次说明；保持完整值可查看，少于分币种最小精度的值不能显示为已计量零。
3. **区分常用内容与管理动作。** 用量顶部是范围 / 来源 / 失败查询，导入 / 清除 / 备用库等放明确的管理入口；备用仅开发续接，不应长期占日常第一页的主要视觉预算。
4. **概览先回答可否用、最近发生什么、有什么待处理。** 新手路径按状态收敛为一个下一步，已有完整配置不反复占同样位置；四档池长尾仍可展开，不强行放大到把最近故障推离首屏。
5. **不同皮肤有各自表面层次，共享任务密度。** 不是给所有地方都加卡片；装饰线用弱角色、可操作边界仍守对比度，中文 / 数字字体搭配与关键动作强弱做整页复测。

如需改默认字体、Industrial 分隔线语气、主编辑面宽度或增加 provider 导航，先给用户看同一 fixture 的原型；不是实施者凭“更现代”直接改规格。

### 6.3 性能结论必须重新测，不能拿旧通过证明好用

本轮没有冷首绘 / font transfer / FPS / IPC 延迟的有效跨项目对照。截图已经等 fonts.ready，参考依赖又不是原锁；字体六态实际 family 只能说明视觉路径，不能说明加载收益。

下一轮至少测：生产自包含构建的冷 / 热首绘、按当前皮肤请求的字体字节、600 模型 / 500 活动 / 30 账户的点击到目标可操作时间、滚动和后台 IPC；保持 UI 必需功能与真实来源数据，不为更小包体删除能力。既有预算增幅与测试数仅是工程记录，未变成 UX 评分。

## 7. 下一轮实施拆分：小活有产物、有边界、有负例

以下是**待主会话采用的派工建议**，未在本研究会话启动实施。先做不依赖新后端的止损，再按任务纵向接通合同；不要让多个会话同时写 `components.tsx`、`App.tsx`、`api.ts`、`mock-bridge.ts` 和词典。用户已要求使用 lightvela-auto 拆小活时，可用独立 session 承接这些有界任务；原有目录所有权和一人发布 Git 的约束仍在。

### 批次 A：先恢复动作可信度与可达性（最高收益，低依赖）

| 小活 | 明确写 scope / 依赖 | 必交行为与负例 |
| --- | --- | --- |
| A1 添加服务商失败保稿 | `Providers.tsx::AddProviderForm`；不改 Rust；独立小活 | await 自己的 save，只有真实 added 且 mounted 才清稿 / 开目标。写失败 / 提交时重复均留原表单，不开旧实体；成功可以继续凭据 |
| A2 输入提交 / Esc / IME | `components.tsx` 的四类输入与 Drawer；共享组件唯一 owner | 一次 Enter 恰一次意图写；blur 不重复；失败原稿可重试；中文合成 Enter 不提交不 blur；文本 Esc 还原而不关层；再退出遵守草稿保护与顶层栈 |
| A3 保存反馈作用域 | `saveQueue.ts`、`App.tsx` 与读 / 观察调用点；依赖 A2 后串行处理共享文件 | 启动 / 余额刷新不产生“配置已保存”；A 的成功不出现在 B；观察失败保留上次有效时间；写排序 / revision / CAS / 后续丢弃通知不退化 |
| A4 添加落点可见 | `Models.tsx`、`Providers.tsx` 的新建入口；新面复用既有组件 | 页头点击后表单 / 目录主控件在视窗并获焦，不在列表 / pager 下方数千 px；取消回触发动作；600 模型正负例不靠脚本 scrollIntoView 代用户找 |
| A5 价格行格式与空间 | `Models.tsx`、对应 `styles.css` 规则、三语；设计微方案先展示 | 去浮点尾数；窄窗不把说明压在 60px 列；未知 / 未配置 / 已知零可区分；不改变计价 / currency / rate 数据；不 whole-tree format |

### 批次 B：把添加 / 账户 / 模型变成连续任务（UI 主批次）

| 小活 | Scope / 前置 | 必交行为 |
| --- | --- | --- |
| B1 类型化实体定位 | `App.tsx` + provider / model / request 的定位参数；A3 后共享 owner | 概览 / 记录可以打开精确实体和子区；关闭 / 返回恢复原查询。旧 deep link 六入口仍可用；不存在实体不偷换同名 |
| B2 账户基本管理入口 | `Providers.tsx`、`Accounts.tsx`、绑定 / Token 控件；B1；先确认管理面布局 | 两模式都可更新该账户面板 Token / Key、看身份与余额原因；常用动作不被 geek gate；高级 header / quirk / selector 按需展开；不新增另一套凭据发布规则 |
| B3 provider → 模型续接 / 分组 | `Models.tsx`、ProviderDrawer；B1 / A4 | 获取目录预选 provider；真正 provider 分组 / filter / 组内操作；保留已选 / query / page；不用每行重复 provider 代替组织；两模式同路径 |
| B4 四档中文与证据可读 | `Models.tsx`、`Routing.tsx`、`Overview.tsx`、词典 | 用途名为主，技术 class ID 在说明 / copy；显示 effort 来源、时间、人工修正与限制；不改 none/minimal、交集、直连未知语义 |
| B5 状态 / 余额口径 | 首页 / provider 行 / Drawer；消费已有 BalanceStatus；B2 | 配置启用和已验证不同；partial 徽章首页不丢；最近尝试 / 最近有效观测分开；aggregate 全失败明确无当前余额；未知不被 null→0 误译 |

B2 是同实体配置与观测聚合，B3 是接受计划的默认按服务商组织；**不需要再次询问用户是否要六入口 / 四档 / 身份隔离**。需要确认的是具体交互承载方案，不是重新打开已接受的需求。

### 批次 C：失败调查与客户端验证闭环（小合同纵向交付）

| 小活 | Scope / 前置 | 必交行为 |
| --- | --- | --- |
| C1 请求总费 / 尝试摘要 | `Activity.tsx`，消费现有 accounting / attempts；A3 | 主费用读 known_cost、可见未知次数，final cost 明示“末次尝试”；失败行保留已知费；多币种分开；identity / binding / protocol / usage evidence 可读 |
| C2 故障直达修复 | `Overview.tsx`、`Activity.tsx` + B1 locator | 最近失败可点击；每尝试到正确账户 / Key / 模型能力 / 价格依据；历史已变更清楚；不自动重放 / 解除钱包 |
| C3 当前默认与原因码 | `api.ts` → `state/takeover.rs` / `agent_takeover/status.rs` → mock；再 `Takeover.tsx` | 回显当前 default / standby，保留=不变；reason_code + 参数用于本地化；预览 / CAS / restore 不退化；不靠前端正则猜新英文 |
| C4 接入主入口与验证面 | `Takeover.tsx`、三语、fixture；C3 | 一条完整接入入口，应用后保留验证面 / 重开说明 / 恢复；unavailable class 能到配置处；正常 mock 也覆盖失败 / stale / conflict，而不只 ready |
| C5 所选 class 观察 | Core Stats / ingress + 桌面证据视图 + API / mock；C3；合同 owner 主导 | 保留认证 / 归因证据强度，增加 class / model / 时间 / 入口信息；非目标 class 不误确认目标；三原生客户端另做获准现场验收 |

C5 不是免费发测试请求的按钮需求。观测用户日常或授权本地 fake 请求即可；真实身份 / AgentRouter effort / 菜单版本等外部证据仍单列。

### 批次 D：用量真正成为一个查询（先本地，后跨来源）

| 小活 | Scope / 前置 | 必交行为 |
| --- | --- | --- |
| D1 接出已有 SQL 分页 | `Activity.tsx` + 已存在 `api.usageHistory`；共享查询 / 视图状态模块；A3 | 本地 >200 条可查询；page / size / total / revision 一致；新筛选取消 / 拒过时答复；返回恢复调查；错误 / 无匹配 / 无数据不同 |
| D2 定义统一查询投影 | API / Core runtime / desktop 合同 owner；D1 可先独立落地 | 不同 record / imported / rollup 的类型、来源、时区、币种、完整性；图 / 小计 / 当前查询导出同口径；备份导出单独命名；不是扩充 recent 来冒充历史 |
| D3 来源状态覆盖与未知 | import adapters / ImportTotals / stats / API / UI 协调；不要独立改共享 schema | 已知成功 / 已知失败 / 状态未知 / completed-only 覆盖分别表达；rollup 有已知计数不能 `ok=null` 全归未知；缓存缺省按来源规范裁定；显示已知费和未知部分 |
| D4 导入结果续接与冲突 | `Activity.tsx` + D2 的来源 / 批次查询；复用原 stage/publish | 结果 → 指定来源 / 批次，明细与汇总可区分，疑似可查看 / 处理；新旧来源幂等、取消、范围、revision 均留回归；预算 / 健康不改变 |
| D5 查询布局确认 / 复测 | 同 fixture 原型 → `Activity.tsx` / `UsageCharts.tsx` / CSS；D2 | 顶部筛选 / 失败查询始终可达；主请求表不先穿过全部账本管理；费用图按币种；年历独立或同窗口三样式按确认的合同落地 |

优先顺序建议：**A1 / A2 → A3 → A4 / A5 → B1 / B2 / B3 → C1 / C2 / C3 / C4 与 D1 → C5 / D2–D4 → 全任务视觉与 native 验收**。B4 / B5 可在相应页面唯一 owner 的切片中带上。默认字体重做、动画路由舞台、额外托盘样式和 ML 研究，不应抢占这轮核心体验主线。

### 7.1 实施前需确认的设计 / 合同取舍

1. **服务商管理面**：保留连接 / 账户总览 tabs，但所有实体动作进统一 Drawer，还是 provider 详情作为宽工作区？推荐先保留六入口与总览，统一实体编辑 / 观测面；不要同时留两套编辑规则。
2. **模型窄窗价格布局 / pager**：数值副行还是列头短数字；列表顶部 / 底部 pager 或有界内部滚动。推荐简单分组 / filter + 可达 pager，先不强制引入虚拟化。
3. **接入证据形状**：配置已写、菜单有效、网关可达、目标 class 已观察分别由什么证据证明，哪些只能由用户确认。UA probable attribution 不能升级成强认证。
4. **年度热力图**：保留独立年度概览，还是三样式完全同范围。现接受合同要求同窗口，但后来记录有全年热力图的明确增量；应协调两者，不静默放宽或单方面删除全年。
5. **视觉语气**：默认 serif、黑分隔线、复杂编辑宽度的候选改动只在用户看到原型后采用。保留当前主题承诺，不借“原生”名义伪造窗口材质 / OS 验收。

原因码、当前选择、按 class 证据、未知覆盖等**满足已接受需求的合同接线**由主会话协调；仅记录形状 / 生命周期需要定稿，不能变成“是否还要真实状态”的重新提问。

## 8. 不应照抄 / 不应误报的边界

1. **参考并非可照抄的业务真理。** Magpie 把“未勾选 effort”解释为不发送 reasoning 字段（`app.js:11730–11759`），并允许 nearest effort / group fixed effort（`routing/client.js:1161–1191`）。Zroutery 明确保留原请求档位、严格筛选原范围；不能照抄省略 / 降档。
2. Magpie 最便宜排序允许无价成员排最后（`routing/client.js:529,739–750`），其“Free for me”也可影响成本偏好；Zroutery 缺价不能进入依赖价格的排序 / 预算，不等价币种不能排序。复制说明结构，不复制价格准入策略。
3. **缓存字段缺失不自动等于未知。** Claude / OpenCode adapter 当前有 optional→0；若源格式规定字段省略就是没有这类 token，这可能是合理规范化。需逐源版本证明“已知零 / 不报告 / 未完成”，再给 completeness / cache_reported；本报告不采纳“所有缺 cache 字段的行必须一律从分母排除”的子结论。当前 live attempt 已有 UsageEvidence，同样不能不读它就猜计量零。
4. **余额 null→0 是防御缺口，不能夸大成当前全部未知归零。** 两个 UI aggregate 分支确有 `remaining ?? 0`（Overview `:428`、Providers `:899`）；当前 `panel_balance_aggregate` 成功分组总是填 Some 数字、无成功则空 map（`state/balances.rs:185–237`），所以尚未证明生产能生成该 nullable 分组。已实在的差距是首页漏 partial / 观测时间、基本修复不相邻；null→0 需加负例防护，而非宣称所有余额已错。
5. **heatmap 稀疏数组错位是潜伏条件。** `heatmapWeeks` 按数组位置切周，无日期连续性校验；当前 production 本地 / standby producer 先铺连续日期，再并入相同窗口导入日（`runtime_store/history.rs:116–132`、`commands.rs:158–163`）。不能写成当前生产已错位；增加 calendar / coverage 后需防空洞与未知误零。
6. **便携 schema v1 不必然丢掉 accounting。** 当前导出序列化整个 RequestRecord，导入允许并校验其 accounting（`stats.rs:1007–1033`）；不是所有 schema v1 都不能表达失败尝试总费。旧文件本来没有这些字段时不能还原缺失事实，独立 attempt 明细备份仍需明确；不要把版本号 alone 当字段不存在的证据。
7. **不能因没有 HTTP 状态就把所有 rollup 都当未知。** CC Switch 日汇总有独立可信 success/failure 计数；Claude completed-only 来源的问题也可能是总体选择偏差，而不是某行完全不知完成。D3 必须按来源覆盖口径裁定。
8. normal mock 的无 setup / capture fixture、假固定 ready 状态、导入累计不变，是研究与测试基建缺口；生产 Rust 存在命令和状态机。任何新增测试先修 fixture 保真，不能从它们推断真实账户 / 客户端状态。
9. CC Switch 也有巨大 Claude 表单 / JSON 高级项、分散授权页与纯图标动作；Magpie 也有长页、自动原生行为和大量能力。参考优势不是“按钮更少”或“卡片更多”，而是任务上下文与结果解释更好。
10. 无真实型号 / effort / 账单证据时，不以“参考项目这么写”认定上游事实。独立后端字段声明与任务成功仍分开；显示与诊断绝不收录秘密 / 正文。

## 9. 验收应怎样改变，才能不再“测试全过但难用”

### 9.1 五任务验收卡，而非页面数验收

- **任务 1**：空安装添加 → Key / 身份 → 余额 / 价格解释 → 获取模型；默认简易、失败重试、不同远端身份、兄弟账户不受影响。
- **任务 2**：600 模型按 provider 找同名模型 → 建议确认 / 待确认 → 改显示名 → 改某档 / 恢复继承 → 解释实际执行；长 ID / 中文 / 缺价 / effort gap。
- **任务 3**：三个原生客户端分别 preview / default / apply / reopen / menu / target-class request / restore；native evidence 与 mock 分开。
- **任务 4**：首页或记录的失败 → 正确 attempt / account / key / capability / pricing / protocol → 返回原查询；失败已知成本、多币种、历史已删实体和不可用诊断。
- **任务 5**：500 条以上本地历史 + 导入明细 / 汇总 / 旧时间 / 未知状态 / 疑似 → 同查询图 / 表 / 导出 → 取消 / 重入 / 迟到更新 / 清除；预算 / 健康不改。

每卡记录“输入 → 点击 → 可见落点 / 当前焦点 → 完成状态 → 失败恢复 → 再进入”，保存截图和必要元数据。820×560 与 1000×720、两模式、长名和相关皮肤覆盖；没有真实平台验收就明确未运行。

### 9.2 新增与保留的门禁

保留既有控件对齐 / 溢出 / overlay / queue / CAS 门禁，同时加：

- 页头添加后第一输入必须与当前 viewport 相交且获得焦点，测试不得先用 `scrollIntoView` 掩盖错误落点。
- 一次可信 Enter 的命令计数恰一；保存拒绝 / 排队丢弃 / 竞态重复时草稿与对象定位符合约定。真实 IME 手工一项独立。
- 新开 / 换实体 / 仅刷新时保存状态不冒领；无测量数字与已知零分别断言。
- 失败筛选首屏可达；实体链接直接打开正确 ID / 子区，返回原调查；查询导出数字 / 来源与当前查询匹配。
- 500 本地记录超过 recent 窗口仍可翻页，revision / 时区日边界 / DST / 同日高精度排序的后端合同继续独立。
- normal `mock-bridge.ts` 与 interaction / layout 两套 harness 都覆盖 client setup / Token / attempt / unknown / partial；不能只补一套就宣称浏览器回归齐全。

当前 `ui_layout_test.py::judge`（`:942–983`）判断的是每行对齐与尺寸；`unfold` 等待表单存在（`:1548–1555`），并不证明表单在首屏。`ui_interaction_test.py::unfold_page_form`（`:1594–1607`）也只确认节点挂载，随后 `element` 会自动滚到元素（`:1400–1414`）。这正是 O2 可以存在而原门禁通过的机制，**不意味着这些门禁无用**。

实施切片后按实际改动运行 `pnpm --dir ui test / build`、UI budget / layout / interaction / overflow；IPC 改动跑 command surface 注册 / ACL；Rust 与持久化改动先定向行为再 repository workspace / MSRV / feature gates，CI Rust 版本由当次 toolchain gate 解析。此研究未运行无关全量构建，也未借主会话历史测试通过作为 UX 结论。

## 10. 交接清单与研究状态

- **研究完成**：五任务比较、具体根因、参考的收益 / 边界、行为负例、视觉密度、受影响文件 / 合同、优先小活与验收在本报告。
- **实现未做**：没有改 UI / Rust；没有改接受计划 / shared evidence registry / node status / DAG；没有新证据编号、knowledge 保存或提交。主会话应先读取本报告，选择批次，按其目录 ownership 组织实施。
- **外部仍开放**：实际 WebView2 / IME / 原生菜单、目标模型 effort / 账户身份 / 有效费率、真实来源含 Codex 正例、安装器、跨平台运行与性能；属于接受计划相应里程碑，不被本轮 mock 观察升级。
- **材料**：上述临时目录含 `drive.py`、`observe_skins.py`、`capture_final_evidence.py`、`attempt_ui_probe.py`、`save_behavior_probe.py`、参考假边界、PNG / JSON。`read-baseline-final.json` 保存读取时的文件 SHA256 / 行数和 Git 状态；例如 Providers=`820fdc34…`、Models=`026cae3d…`、Components=`75b3eaaf…`、Activity=`03baea68…`。后续变化须重新核对符号。
- **复现关键负例**：对自有 mock tab 的 `window.__TAURI_INTERNALS__.invoke` 临时包装 `save_config` 拒绝 / 计数；不要改真实桥、sourceplan、原账户或客户端文件。600 模型点击证据可用 `drive.py` 在自己的 profile 重开 `mock.html?stress=600`，量表单位置和 activeElement。参考后端数据永远只是 fake。
- **验证 / 资源收尾**：报告 UTF-8 / 五任务章节 / 无尾随空白通过，workspace `git diff --check` 退出 0（有已有文件 LF/CRLF 提醒）；报告为 untracked，index 仍空，未提交。四个研究 Chrome 均通过各自 `Browser.close` 关闭；自建参考服务 5198、51168、50857 已关闭，先前 50788 也已自行结束；5186 原 mock 继续监听。未对 5173、运行应用 / 网关或他人 tabs 执行关闭；收尾时 5173 没有监听属其他会话运行态，不是本研究停止。临时证据保留，不递归清理用户路径。

最终判断：**下一轮值得交付的不是“又优化了几页”，而是用户在五条任务里不再自己补齐上下文、不再猜保存 / 生效 / 金额、不再为一个下一步滚到数屏以外。** 先以 A 批次修动作可信，再以 B / C / D 贯通任务，视觉调整为这条主线服务。
