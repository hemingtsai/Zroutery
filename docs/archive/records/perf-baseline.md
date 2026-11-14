# 前端性能基线（UI-UX-P2）

> 测量任务产物：只记录数字，不含任何优化改动。master plan 铁律「未测量不优化」的 M0 前端基线。
> 所有数字基于 `?stress=600` 压力 fixture（600 模型 / 13 提供商 / 500 记录 / 30 账户）。

- 测量时间：2026-10-31
- 执行：ZCode agent（UI-UX-P2 节点）
- 报告状态：基线已建立；后续优化（M4 前端批次）以此为 before 参照

## 1. 测量环境

| 项 | 值 |
| --- | --- |
| 机器 | Windows 10.0.26200 x64，20 逻辑核，32 GB RAM |
| 浏览器 | ZCode 内置浏览器（Electron 44.7.0 / Chromium 152.0.7977.130），UA `ZCode/3.17.1` |
| 显示 | 60 Hz，devicePixelRatio 1.25 |
| 应用 | Vite dev server `http://localhost:5173/mock.html?stress=600`（未压缩模块图）+ `pnpm --dir ui build` 产物（压缩分包） |
| 区域 | zh-CN（i18n 中文词典在首屏包内） |
| fixture | `?stress=600`：600 模型（13 提供商 × 4 tier + 1 个无 tier）、500 条 recent（5 类客户端归属）、30 账户（20 启用）、modelHealth 600 条。默认无参数时 fixture 保持 8 模型 / 3 提供商 / 120 记录 / 2 账户，已实测确认未受影响。 |

fixture 改动只在 `ui/mock-bridge.ts`（mock 专属，不进产品路径，`vite build` 不打包它），由 URL 参数唯一触发；压力会话使用独立的 sessionStorage 配置槽（`zroutery-mock-config:stress-600`），与常规 mock 会话互不污染。

## 2. 方法与可复现步骤

每次测量 ≥3 次取中位数，同时记录最差值。完整复测流程：

1. `pnpm --dir ui dev`（5173 端口）。
2. 浏览器打开 `http://localhost:5173/mock.html?stress=600`。**窗口必须保持前台**：页面 `visibilityState === "hidden"` 时 Chromium 停止渲染（rAF 不触发、FCP 不产生），且 App 自身会跳过轮询（`App.tsx` 的 visibility guard），所有时序数字都会失真。
3. 首屏：每次 `goto` 一个带 `run=N` 的新 URL → 等待 `domcontentloaded` → 页面内 setTimeout 轮询，直到「FCP 条目存在 + 6 个导航按钮 + `.page` 内容 ≥2 块」，读取 `performance.getEntriesByType('paint'|'navigation')`。run1 为半冷启动（浏览器 HTTP 缓存空一次），run2-5 为热启动。
4. 切页延迟：页面内 `el.click()`（不要用外部 locator click，2 秒轮询页上会超时）。
   - `titleMs`：MutationObserver 捕获 `.mainbar .page-title` 文本变化（微任务级精度，不受节流影响）。
   - `contentMs`：点击到「Suspense fallback 消失 + 页面内容就绪（models 页要求 ≥580 行 `.list-row`）」的 rAF 确认时间。
   - 页面经 `React.lazy` 懒加载：冷 = 每个文档内首次访问（含模块拉取），热 = 同文档内重复访问。
5. 滚动帧率：模型页 `.content` 容器，rAF 驱动 `scrollTop` 线性扫过全程 2 秒，采样帧间隔分布。
6. 轮询开销：PerformanceObserver `longtask` + 包装 `window.__TAURI_INTERNALS__.invoke` 计时，Overview 页 14s 窗口、Activity 页 30s 窗口；另用 `document.dispatchEvent(new Event('visibilitychange'))` 触发 5 次即时轮询验证单次渲染成本。
7. 构建：`pnpm --dir ui build`，对 `dist/assets` 全量文件用 `zlib.gzipSync(level 9)` 算 gzip。
8. 生产网络成本：`pnpm --dir ui preview --port 4173`。注意：dist 不含 mock.html（构建只跟随 index.html），无 Tauri runtime 时生产页停在 ErrorBoundary（"Cannot read properties of undefined (reading 'invoke')"），因此**生产态 App 首绘时间本基线不测**（需 Tauri 窗口），只记录入口网络传输量（ErrorBoundary 页面与真实首屏的网络成本一致）。

## 3. 构建分包（`pnpm --dir ui build`，gzip level 9）

| chunk | raw | gzip |
| --- | ---: | ---: |
| react-vendor | 185.2 kB | 57.5 kB |
| index（App + i18n zh-CN 词典） | 162.5 kB | 53.5 kB |
| vendor（radix 等 node_modules） | 82.8 kB | 28.3 kB |
| i18n.zhTW（懒加载） | 57.3 kB | 20.1 kB |
| Routing（懒） | 36.3 kB | 8.8 kB |
| Providers（懒） | 33.1 kB | 8.5 kB |
| Activity（懒） | 27.6 kB | 7.8 kB |
| Models（懒） | 17.4 kB | 5.1 kB |
| Accounts（懒） | 12.1 kB | 3.7 kB |
| Settings（懒） | 11.5 kB | 2.3 kB |
| Takeover（懒） | 10.6 kB | 2.8 kB |
| Cutover（懒） | 6.9 kB | 2.1 kB |
| Overview（懒） | 5.0 kB | 1.3 kB |
| AccountBindingReview（懒） | 1.3 kB | 0.6 kB |
| rolldown-runtime | 0.6 kB | 0.4 kB |
| **JS 合计（15 chunks）** | **650.0 kB** | **202.9 kB** |
| index.css | 33.4 kB | 6.9 kB |
| vendor.css | 98.5 kB | 34.9 kB |
| **CSS 合计** | **131.9 kB** | **41.8 kB** |
| 字体 woff2（105 个文件，见 §7） | 5932.4 kB | ≈同 raw |

首屏关键路径（冷加载实际传输，生产 preview 实测）：JS 4 chunks（react-vendor + index + vendor + runtime）= 441.4 kB decoded / **144.6 kB 传输**；CSS 2 个全部加载；**首绘即拉 4 个字体子集 262.7 kB**。首绘网络总量 ≈ **445 kB（wire）**，其中 JS+CSS gzip ≈ 182.8 kB。

对照旧基线 `dist-size.txt`（index 单 chunk 278 kB 时代）：分包策略已把最大 chunk 降到 185.2 kB，预算门禁（单 chunk ≤320 kB / JS ≤700 kB）通过。

build 输出尾部：

```
dist/assets/vendor-C9eQn-5-.js                                     84.79 kB │ gzip: 29.25 kB
dist/assets/i18n.zhTW-BxsBt1U9.js                                  58.67 kB │ gzip: 20.69 kB
dist/assets/index-DsNuc8oa.js                                     166.41 kB │ gzip: 55.16 kB
dist/assets/react-vendor-BfBh8zT8.js                              189.60 kB │ gzip: 59.61 kB
✓ built in 585ms
```

（tsc --noEmit 通过，含 stress fixture 类型检查。）

## 4. 首屏（dev 模式，5 次取中位/最差，ms）

| 指标 | 820×560 中位 | 820×560 最差 | 1280×800 中位 | 1280×800 最差 |
| --- | ---: | ---: | ---: | ---: |
| DCL end | 177 | 325 | 194 | 271 |
| FCP（首绘，即 shell 可见） | **220** | 352 | **244** | 312 |
| load end | 180 | 328 | 199 | 274 |
| 首屏可交互确认（FCP+导航+内容齐备，轮询上限 2.4s 内全部达成） | ~250 | ~410 | ~270 | ~330 |

- run1 为半冷启动（每次会话第一跳），中位数由 run2-5 决定；dev 模式 JS 模块图未压缩（首屏 10 个文件共 3.92 MB decoded），生产包体积见 §3。
- FCP≈shell 提交 + 首帧；KPI 数据（get_snapshot，含 600 模型）在 mount 后一个微任务周期内就绪，FCP 后 ~30ms 内容齐备。
- 最差值全部来自 run1（首次导航），无异常离群。

## 5. 切页延迟（dev 模式，stress=600，ms）

冷 = 每个文档内首次点击该页（含 React.lazy 模块拉取；dev 下是 vite 按需模块图，生产是小 chunk，冷值会明显更低）。热 = 同文档重复点击，3 轮取中位/最差。`contentMs` 是「fallback 消失 + 内容就绪」的用户可感时间。

**冷切换 contentMs（各 1-2 次）**

| 页面 | 820×560 | 1280×800 |
| --- | ---: | ---: |
| providers | 317 | 323 |
| models | 317 | 326 |
| takeover | 346 | 352 |
| activity | 344 | 315 |
| settings | 318 | 317 |

**热切换 contentMs（中位 / 最差）**

| 页面 | 820×560 | 1280×800 | M6 交互目标 16ms |
| --- | ---: | ---: | --- |
| overview | 16.0 / 16.8 | 19.4 / 20.1 | ✅ 边缘达标 |
| providers | 7.7 / 7.8 | 10.8 / 19.0 | ✅ |
| models | **235.1 / 259.6** | **285.5 / 308.2** | ❌ 超标 ~15-19 倍 |
| takeover | 39.7 / 53.9 | 39.3 / 53.9 | ❌ |
| activity | **82.3 / 84.8** | **80.8 / 98.0** | ❌ |
| settings | 23.2 / 28.2 | 22.8 / 25.4 | ⚠️ 轻微超标 |

热切换时 `.page-title` 提交（titleMs）中位：models 88.3（1280 下 126.5，最差 156）、takeover 23.9-37.3、activity 23.6-35.9、settings 13.4-24.4、providers 3.5-9.8、overview 5.3-7.4。即 models 页 600 行的 React 渲染提交本身就要 88-156 ms，其后还有 ~100-150 ms 的 54 778 px 高列表布局/绘制。

## 6. 滚动帧率（模型页 600 行，2 秒 rAF 采样，3 次）

| 指标 | 820×560 | 1280×800 |
| --- | ---: | ---: |
| FPS（中位） | **60.5** | **61.0** |
| FPS（最差一轮） | 60.5 | 60.5 |
| 平均帧时长 | 16.67 ms | 16.67 ms |
| 帧时长 p95 | 16.8 ms | 16.8 ms |
| 最差帧 | 16.9 ms | 17.3 ms |
| >32 ms 长帧数（3×2s 合计 364 帧） | 0 | 0 |
| 行数 / 滚动区间 | 600 行 / 54 255 px | 600 行 / 35 649 px |

**600 行 55fps+ 目标：基线即达标**（合成滚动，无 React 状态变更路径）。注意这只证明渲染合成不卡帧；交互开销瓶颈在切页渲染（§5），不在滚动。

## 7. 字体传输

| 项 | 值 |
| --- | --- |
| dist 内 woff2 总量 | 105 个文件 / 5 932 kB（woff2 已压缩，gzip 无增益） |
| Noto Serif SC variable | 100 个 unicode-range 分片 / 5 884 kB（单分片 76.9-103.8 kB） |
| IBM Plex Mono | 5 个文件 / 48.0 kB |
| IBM Plex Sans | **0 个文件**（package.json 声明了 `@fontsource/ibm-plex-sans` 但 dist 无产物，疑似未使用或未导入，可清理） |
| 首绘实际加载（生产冷启动实测） | **4 个文件 / 261.5 kB decoded / 262.7 kB wire**（≈1 个 latin + 2-3 个 CJK 分片 + mono） |
| dev 全会话累计加载 | 17 个文件 / 1 091 kB（浏览更多中文内容后增加的分片） |

首屏网络总量里字体占 ~60%（262.7 / 445 kB）。unicode-range 分片本身工作正常（按需拉取），但首绘即需 3 个 CJK 大分片（~80 kB each）。

## 8. Activity 2 秒轮询开销

| 场景 | 结果 |
| --- | --- |
| Overview 页 14s 窗口（22 次 get_activity） | invoke 耗时中位 1.1 ms（0.7-2.8）；**长任务 0** |
| Activity 页 30s 窗口（17 次轮询）+ 5 次强制轮询 | invoke 中位 ~1.2 ms（0.7-4.0）；稳态**长任务 0**（全部 <50 ms） |
| Activity 页 mount 后首屏全表渲染（8 轮窗口内捕获 1 次） | **154 ms 长任务 1 个**（500 记录表 + 图表首次整表渲染，非稳态轮询） |

结论：稳态轮询开销很低（mock fixture 每次 get_activity 重建 600 条 health + 500 条记录也只 ~1-2 ms；真实后端为 IPC + 序列化，成本口径不同）。轮询相关的真实风险是**数据到达后的整表重渲染**：mount 后第一次渲染出过长任务 154 ms，稳态轮询未再出现 >50 ms，但每次轮询仍替换 500 条记录数组并触发整表 re-render（§5 中 activity 热切换 80 ms 与此同源）。

## 9. 与 master plan M6 目标对照

| M6 目标 | 基线实测 | 结论 |
| --- | --- | --- |
| 600 行 55fps+ | 60.5-61 fps，0 长帧 | ✅ 已达标（滚动路径） |
| 常规 UI 60fps | 60.5-61 fps | ✅ 已达标 |
| 交互 80→16ms | overview 16/19 ms、providers 8/11 ms 达标；settings 23 ms、takeover 39 ms、activity 81/82 ms、**models 235/286 ms** | ❌ models 与 activity 是主要缺口，models 超标 ~15-19 倍 |
| 首屏体积 -40–60% | 基线：entry JS+CSS gzip 182.8 kB；首绘网络总量 ≈445 kB（含 262.7 kB 字体） | 📌 基线已记录；-40% ≈ 110 kB、-60% ≈ 73 kB（对 JS+CSS）为后续验证口径 |

## 10. 最差项 Top3 与优化候选排序建议

1. **Models 页 600 行全量 DOM（无虚拟化）** — 热切换 235-308 ms（M6 目标 16 ms），其中 React 提交 88-156 ms + 大列表布局绘制 ~100-150 ms；600 个 `.list-row`、54 778 px 高。→ 首选虚拟化/窗口化渲染（master plan B3 已规划「Models 目录 >200 行与 600 行基准验证虚拟化/过滤」），或至少把行内昂贵子树（每行 31 个状态点等）延迟到可视区。
2. **Activity 页 500 记录表 + 轮询整表替换** — 热切换 60-98 ms，mount 首渲出现过 154 ms 长任务，每 2 s 轮询替换整表数据源。→ 按热点 memo、分页/窗口化（B3 同款），轮询 diff 或冻结列。
3. **字体首绘 262.7 kB（占首屏网络 ~60%）** — 首绘即拉 3 个 ~80 kB 的 CJK variable 分片；dist 全量 5.88 MB/100 分片躺在盘上。→ `font-display: swap` 现状确认 + 首屏只保留必要分片/延迟非首屏分片；顺带清理未使用的 `@fontsource/ibm-plex-sans` 依赖。

次优先：takeover 39 ms、settings 23 ms（组件级排查即可）；providers（7.7 ms）与 overview（16 ms）达标不动。

## 11. 复测注意（下次照做）

- 必须窗口前台（见 §2.2 的 visibility 陷阱；页面 hidden 时 rAF=0、FCP 不产生、轮询被 App 跳过）。
- 用页面内 `el.click()` + MutationObserver/rAF 采样，不要用外部 locator click（轮询页会超时）。
- dev 冷切页 ~320 ms 是 vite 按需模块图所致，不能与生产冷加载混比；生产冷加载需 Tauri 窗口（preview 无 bridge 会停在 ErrorBoundary）。
- 改动 `ui/mock-bridge.ts` 后重测前，先在页面里确认 `get_snapshot` 返回 600 模型 / 500 记录 / 30 账户，防止 sessionStorage 旧配置串味（压力会话已用独立 key 隔离）。
- 轮询长任务采样 ≥30 s 再下结论（单次 154 ms 峰值出现在 mount 后首渲，稳态 17 轮 0 长任务）。
