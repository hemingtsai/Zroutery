# 皮肤规格 v2：MD3 / Fluent（官方规范对照实现）

> v1 被 UI 负责人否决（"md3不md3 fluent不fluent"）：色板+最小覆写不构成皮肤。
> 本 v2 从官方规范提取具体值，**组件按各自设计语言的形态实现**。
> 来源：m3.material.io（Color roles / Shape scale / Elevation / State layers）、
> fluent2.microsoft.design（Shapes / Design tokens）、@fluentui v9 web 主题中性色。
> 共同守恒：`--control-h` 30px；industrial 零改动；强调色=系统色（AccentColor 级联，回退链已建）。

## 一、MD3（官方 baseline + shape scale + state layers）

### 色 roles（Theme Builder baseline，light）
| role | hex | 用于 |
|---|---|---|
| primary / on-primary | #6750A4 / #FFFFFF | 主按钮、选中态 |
| primary-container / on-primary-container | #EADDFF / #21005D | tonal 按钮、激活 pill |
| secondary-container / on-secondary-container | #E8DEF8 / #1D192B | 导航激活、segment 选中 |
| surface | #FEF7FF | 页面底 |
| surface-container-low / -container / -high / -highest | #F7F2FA / #F3EDF7 / #ECE6F0 / #E6E0E9 | 卡片层次、输入底、hover 层 |
| on-surface | #1D1B20 | 主文字 |
| on-surface-variant | #49454F | 次文字、hint |
| outline | #79747E | 输入框描边（强线） |
| outline-variant | #CAC4D0 | 分隔线（弱线） |
| error / error-container / on-error-container | #B3261E / #F9DEDC / #410E0B | 危险 |

### dark
| role | hex |
|---|---|
| primary / on-primary | #D0BCFF / #381E72 |
| primary-container / on-primary-container | #4F378B / #EADDFF |
| secondary-container / on-secondary-container | #4A4458 / #E8DEF8 |
| surface | #141218 |
| surface-container-low / -container / -high / -highest | #1D1B20 / #211F26 / #2B2930 / #36343B |
| on-surface | #E6E0E9 |
| on-surface-variant | #CAC4D0 |
| outline / outline-variant | #938F99 / #49454F |
| error / error-container | #F2B8B5 / #8C1D18 |

### Shape scale（官方）
extra-small **4px**（文本框/自动审查行）、small **8px**（chip/segment）、medium **12px**（卡片/KPI tile）、large **16px**（侧抽屉）、extra-large **28px**（对话框）、full **999px**（按钮/开关）。

### State layers（官方：悬浮色 = on-color 的透明度层，不是预调灰）
hover = on-color **8%**；pressed/focus = **12%**；dragged = 16%。
实现：`color-mix(in srgb, var(--on-surface) 8%, transparent)`（表格行/列表项）；
控件内 state layer 用各自身上的 on-color（按钮 hover = on-primary 8% 叠加）。

### 组件映射
- 按钮：full pill；filled=primary/on-primary；tonal（default）=primary-container/on-primary-container；ghost=text 按钮（primary 文字）
- 输入框/Select：extra-small 4px，描边=outline，聚焦 2px primary
- segment（tab）：small 8px，选中=secondary-container 填充 + on-secondary-container 文字（去下划线）
- 侧栏导航项：full pill，激活=secondary-container
- KPI tile：medium 12px，底=surface-container-low，替代发丝线
- 抽屉：large 16px 起始侧 + elevation level2 阴影
- 对话框/横幅：extra-large 28px
- 表格：平，行 hover=on-surface 8%
- Badge/chip：small 8px

### Elevation（官方 level1/2）
level1 = `0 1px 2px rgba(0,0,0,.30), 0 1px 3px 1px rgba(0,0,0,.15)`；level2 = `0 1px 2px rgba(0,0,0,.30), 0 2px 6px 2px rgba(0,0,0,.15)`（抽屉/浮层）。

## 二、Fluent 2（官方 Shapes + v9 web 中性色 + 品牌 #0F6CBD 系）

### 中性色（light，@fluentui v9 web 主题）
| token | hex | 用于 |
|---|---|---|
| colorNeutralBackground1 | #FFFFFF | 页面/卡片 |
| colorNeutralBackground2 | #FAFAFA | 次级面 |
| colorNeutralBackground3 | #F5F5F5 | 页面底（canvas） |
| colorNeutralBackground4 | #F0F0F0 | hover |
| colorNeutralBackground1Selected | #E8E8E8 | 选中 |
| colorNeutralForeground1 | #242424 | 主文字 |
| colorNeutralForeground2 | #424242 | 次文字 |
| colorNeutralForeground3 | #616161 | hint |
| colorNeutralStroke1 | #D1D1D1 | 控件描边（强线） |
| colorNeutralStroke2 | #E0E0E0 | 分隔线（弱线） |
| brand: background | #0F6CBD（hover #115EA3，pressed #0C3B5E） | 主按钮 |
| brand: foreground | #115EA3 | 链接、选中文字 |
| error | #B10E1C | 危险 |

### dark
| token | hex |
|---|---|
| colorNeutralBackground1 | #292929 |
| colorNeutralBackground2 | #1F1F1F |
| colorNeutralBackground3 | #141414 |
| colorNeutralBackground4 | #191919 |
| colorNeutralForeground1 | #FFFFFF |
| colorNeutralForeground2 | #D1D1D1 |
| colorNeutralForeground3 | #A0A0A0 |
| colorNeutralStroke1 | #5C5C5C |
| colorNeutralStroke2 | #3D3D3D |
| brand: background | #115EA3（hover #2899F5） |
| error | #F1707B?（按 v9 dark colorStatusDangerForeground1 校正） |

### Shapes（官方 fluent2.microsoft.design/shapes）
控件 **4px**、卡片/面 **8px**、对话框 **12px**。全 skin 的 `--control-h` 30px 不变。

### 组件映射
- 按钮：4px；primary=brand 实心/on-primary；default=#FFFFFF(1) 底 + stroke1 边，hover=bg4；焦点=Fluent 双环（1px 前景内环 + 2px 品牌外环，gap 用 box-shadow）
- 输入框：4px + stroke1 边；聚焦=底边 2px brand（Fluent 文本框签名）+ 描边转 brand
- tab（segment）：pivot 短条——选中项底部 3px 圆头 16px brand 条（absolute，零高度影响）
- 侧栏：acrylic——`color-mix(in srgb, var(--bg) 78%, transparent)` + `backdrop-filter: blur(24px) saturate(1.3)`；导航项 hover=bg4、选中=bg1Selected
- 抽屉/toast/对话框：12px（对话框）/8px（toast）+ Fluent 阴影 shadow16=`0 8px 16px rgba(0,0,0,.14)`
- 表格：行 hover=bg4；无圆角（Fluent 数据表平）
- Badge：4px

## 三、两 skin 共同的组件清单（层3 覆写落点）
.btn（primary/default/ghost/danger 四变体）、.segment-item、.nav-item、输入框/Select、
.drawer、.banner、.badge/.chip、.kpi-cell、.pager 按钮、.toast、滚动条（fluent 细滑）、
.checkbox（md3 2px / fluent 4px）。
**不做**：字体（后续与首绘合并）、switch 改形、图表 token（color-mix 自动随 --text）。
