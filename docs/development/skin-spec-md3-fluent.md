# 皮肤规格 v3：MD3 / Fluent（官方实现级对照）

> v1 被否（"md3不md3"：色板+最小覆写不构成皮肤）；v2 按手抄 token 表实现，v3 复核发现
> 表中多处值/方向错误与整块组件缺口，本轮改为**逐值对照官方实现源**：
> - MD3：`material-components/material-web` `tokens/versions/v0_192/_md-comp-*.scss`、
>   `material-foundation/material-tokens`（state.css / elevation.css）、
>   Flutter `color_scheme.dart`（现行 baseline hex）。
> - Fluent：`microsoft/fluentui` `packages/tokens/src`（alias/global/brandColors/shadows）、
>   各组件 griffel 样式（react-button / react-input / react-card / react-dialog /
>   react-menu / react-toast / react-tabs / react-checkbox / react-switch）、
>   learn.microsoft.com WinUI 焦点/字体规范。
> m3.material.io 与 fluent2.microsoft.design 页面为 JS 渲染，值以上述机器可读源为准。
>
> 共同守恒：`--control-h` 30px；`.segment-item` 36px；开关占位 36×20（skin 只改轨/块
> 形状）；Industrial 形状/语义零改动；MD3/Fluent 强调色读 native `systemAccent()`，
> 不可用时保留官方品牌回退。图表 token（color-mix 随 --text）机制不变。

## 一、MD3

### 色 roles（现行 baseline，2023 surface-container 方案；light/dark）
| role | light | dark |
|---|---|---|
| primary / on-primary | #6750A4 / #FFFFFF | #D0BCFF / #381E72 |
| primary-container / on | #EADDFF / #21005D | #4F378B / #EADDFF |
| secondary-container / on | #E8DEF8 / #1D192B | #4A4458 / #E8DEF8 |
| surface | #FEF7FF | #141218 |
| surface-container-low / -container / -high / -highest | #F7F2FA / #F3EDF7 / #ECE6F0 / #E6E0E9 | #1D1B20 / #211F26 / #2B2930 / #36343B |
| on-surface / on-surface-variant | #1D1B20 / #49454F | #E6E0E9 / #CAC4D0 |
| outline / outline-variant | #79747E / #CAC4D0 | #938F99 / #49454F |
| error 系 | #B3261E / #F9DEDC / #410E0B | #F2B8B5 / #8C1D18 / #F9DEDC |
| inverse-surface / -on / -primary | #322F35 / #F5EFF7 / #D0BCFF | #E6E0E9 / #322F35 / #6750A4 |

**tonal 按钮 = secondary-container**（material-web filled-tonal-button；v2 误用 primary-container）。
组件映射落点：`--surface`=container-low（页面/抽屉）、`--surface-muted`=container（菜单容器）、
`--surface-high`=container-high（对话框）、`--container-highest`=container-highest（开关未选轨）。

### Shape scale（官方逐组件）
extra-small **4px**：文本框、菜单/snackbar/tooltip、checkbox(2px 属 fluent 例外见下)；
small **8px**：chip/badge 标签；medium **12px**：卡片/KPI tile；large **16px**：modal drawer
端角；extra-large **28px**：对话框/横幅；full：按钮、分段按钮组、导航指示、开关、菜单项
内部高亮=**方角**（list item）。checkbox：md3 2px。

### State layers
hover **8%** / focus·pressed **12%** / dragged 16%，叠加色 = 该容器自身的 on-color。
**主按钮 hover = on-primary 8% → 浅色主题变亮**（v2 曾用 85% accent+text 变暗——那是
Fluent brand ramp 的方向，不是 M3 state layer）；深色 = 深色 on-primary 8% → 变暗。

### Elevation
light：level1 `0 1px 2px rgba(0,0,0,.30), 0 1px 3px 1px rgba(0,0,0,.15)`；level2
`0 1px 2px .30, 0 2px 6px 2px .15`；level3 `0 1px 3px .30, 0 4px 8px 3px .15`。
分工：抽屉（modal drawer）= level1、菜单/popover = level2、对话框/snackbar = level3。
**dark：官方用 surface tint 而非阴影**——primary 按层 5%/8%/11% 叠加到各自容器
（drawer/popover/confirm 各一条 color-mix），三档阴影 token 全部置 none。

### 组件要点（对 UI 的具体裁定）
- 输入框：resting 描边 outline 1px → **hover on-surface 1px** → focus primary 2px（内绘）。
- 分段控件 = 官方 segmented button：组 1px outline（inset box-shadow 零位移）+ 药丸端 +
  选中 secondary-container；去掉下划线轨。
- 下拉/菜单：容器 surface-container、4px、level2、上下 8px 内边距、item 方角、选中项
  secondary-container 对。
- 对话框：surface-container-high + level3 + 28px，去边框。
- toast = **snackbar**：inverse-surface 底 + inverse-on-surface 文字 + 4px + level3；
  本应用 toast 无动作链接，inverse-primary 留 token 未消费。
- 开关（v3 决定改形，v2 曾"不做"）：药丸轨 = container-highest + 2px outline，圆块 =
  outline 色，hover 块 on-surface-variant；选中轨 primary、块 on-primary，hover 块
  primary-container；块 12→14px（官方 16→24 等比入 20px 轨）。
- KPI tile：12px + surface-container-low 底，发丝线去除。

### ok/warn（自定义对，官方容器模式推导）
baseline 无 success 角色（color roles 仅 primary/secondary/tertiary/error+中性）。
按 baseline error 对的 tone 结构生成：light tone 40/90（ink/container，OKLCh h148/h72、
chroma 对齐 error 对）：ok `#00722A` / `#DAECDB`（4.94:1 on bg、5.80 on surface）、
warn `#835100` / `#F5E3CD`（5.33:1 on bg、6.35 on surface）；dark tone 80/30：
ok `#93D69C` / `#1B3820`（7.56 / 10.92）、warn `#EAB26B` / `#432B07`（6.98 / 9.81）。
danger 保持官方 error 对。

## 二、Fluent

### 中性色 v9 web（light/dark，含 v3 对 v2 表的修正）
| token | light | dark |
|---|---|---|
| Background1（卡/控件底） | #FFFFFF | #292929 |
| Background2 | #FAFAFA | #1F1F1F（**= 深色画布**，≈WinUI mica） |
| Background3（inset 填充） | #F5F5F5 | #141414 |
| Background1Hover / 1Selected | #F5F5F5 / #E8E8E8 | #3D3D3D / #383838 |
| Foreground1/2/3 | #242424 / #424242 / #616161 | #FFFFFF / #D1D1D1 / #A0A0A0 |
| Stroke1 / Stroke2 | #D1D1D1 / #E0E0E0 | #666666 / #525252（v2 表把 #3D3D3D 即 Stroke3 当 Stroke2） |
| Stroke1Hover/Pressed | #C7C7C7 / #B3B3B3 | —（皮肤按控制线 #808080 亮端外推 #999999/#ADADAD） |
| brand Background(hover/pressed) | #0F6CBD（#115EA3/#0C3B5E） | #115EA3（#0F6CBD/#0C3B5E） |

v2 的两处方向性错误：**深色 hover 填充**用 #191919（=Background4 旧值，向下沉）——官方/
WinUI 深色悬停**变亮**，v3 用 WinUI 合成模型 `rgba(255,255,255,.0605)`（卡上 ≈#3D3D3D、
画布上 ≈#2D2D2D）；**深色画布** #141414 → #1F1F1F。控件描边为本 skin 特有的 WCAG 3:1
加深版 #8A8A8A/#808080（≈WinUI ControlStrokeDefault black .44 的实测域），hover/pressed
沿 v9 ramp 方向从它外推（light 变深 #6E6E6E/#565656，dark 变亮）。

### Shapes（v9 borderRadius 令牌：small 2 / medium 4 / large 6 / xLarge 8）
按钮/输入/菜单 flyout/toast/tooltip/Card=**4px**；**DialogSurface=8px（xLarge，v2 表写 12px 有误）**；
checkbox 指示器 **2px**（borderRadiusSmall，v2 写 4px 有误）；开关轨/块=圆；badge rounded=4px。
抽屉（大浮层）沿 flyout 家族 8px 端角 + shadow16。菜单/popover 边框 = 透明。

### Shadows（v9 createShadowTokens：ambient .12 / key .14，dark .24/.28）
shadow4 `0 0 2px A, 0 2px 4px K`（KPI 卡）；shadow8 `0 0 2px A, 0 4px 8px K`（toast、tooltip、
卡 hover）；shadow16 `0 0 2px A, 0 8px 16px K`（抽屉/菜单/popover，v2 漏了 ambient 层）；
shadow64 `0 0 8px A, 0 32px 64px K`（对话框，v2 用了 flyout 阴影，修正）。

### 组件要点
- 主按钮：brand 底/on-brand 字，hover/pressed 沿 brand ramp（系统强调色 = 85% accent+text
  加深 / 40% accent+white 提亮的既有换算保持）。
- 输入框：stroke1 边 → hover 沿 ramp 变深；聚焦 = 底边 2px brand 签名 + 描边转 brand（保持）。
- **Tab/pivot（v3 修正）**：选中文字 = Foreground1（**不是**品牌墨色；v2 误用
  colorBrandForeground1），指示条 = 3px 圆头、**左右 inset 12px 近全宽**（v2 的 16px 短条
  有误），色 = compoundBrandStroke → 系统强调色。
- KPI 卡（v3 补齐，v2 漏做）：v9 Card.filled = Background1 + 4px + shadow4，hover shadow8，
  发丝线去除。
- toast（v3 修正）：**4px**（v2 写 8px）+ 透明边框 + **shadow8**（v2 用 shadow16）；
  左侧 3px 语义条保留（Infobar 式风格化，注明非官方结构）。
- 开关（v3 决定改形）：未选 = 透明轨 + 1px 可及描边 + 灰块（--muted）；选中 = brand 轨 +
  反色块；块 12→16px（官方 12→20 等比入 20px 轨）。
- 侧栏 acrylic（78%+blur24）保持；焦点双环保持（WinUI 2px 外环 + 1px 内环）。

### ok/warn（官方 v9 令牌）
success `#0E700E`/`#F1FAF1`（dark `#54B054`/`#052505`）、warning `#BC4B09`/`#FFF9F5`
（dark `#FAA06B`/`#4A1E04`）、danger `#B10E1C`/`#FDF3F4`（dark `#DC626D`/`#3B0509`）。
ink 自身 tint 上：5.89 / 4.85 / 6.55（dark 6.06 / 6.97 / 4.96）；白面上 6.28 / 5.06 / 7.12。

## 三、系统强调色（沿 v2 决定，记录于 styles.css）

`get_system_accent()` → `--system-accent` / `-soft` / `-fg`；light 用原色（与 OS 按钮同保证），
dark 45% accent 混白做 tone-80 反转 + on-primary 15% accent 混墨、hover 40%。**状态色不跟随
强调色**。对比度不向用户自选强调色承诺：v3 起主按钮 hover 是 on-primary state layer，回退
色板全部 ≥4.5:1；裸系统 accent 的配对即 OS 自己的配对（white on #0075FF = 4.21:1、on
#8E5EDF = 4.36:1——Windows 自身数字）。

## 四、明确不做 / 保持
- `--control-h` 30px、`.segment-item` 36px、开关 36×20 占位（布局桩有效性边界）。
- 图表零规则（color-mix 随 --text 自动随皮肤）。
- Industrial（默认）皮肤零改动；皮肤规则全部 skin-scoped。
- pager 箭头是 .linky 文本动作，不入组件矩阵。
- M3 的 40px 控件高度、字号 ramp 不搬（本应用 30px/14px 网格是跨 skin 守恒量）。
