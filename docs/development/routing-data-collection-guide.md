# Zroutery 路由数据获取指南

> 面向 Zroutery 的高强度使用者。目的是从你自己的真实使用中导出一份可用于
> **离线回放（replay）与路由模型评估**的数据集。
>
> 本文所写的文件名、字段名、配置键、接口路径均已对照当前 `dev` 分支源码核实，
> 不是推测。若你手上的版本不同，请以 `crates/zroutery-core/src/ml/traces.rs`
> 与 `crates/zroutery-core/src/config.rs` 为准。

---

## 0. 一句话原理

Zroutery 的 ML 学习能力**不是从你的日志里"挖"出来的，而是它自己在服务请求时
顺手写下来的**。只要两个开关打开，每次请求结束都会往一个 JSONL 文件里追加一条
记录，那条记录里同时含有：

- 服务端**当时计划**（decision-time）的候选集合和每个候选的特征向量
- 服务端**实际发生**的每一次上游尝试（attempt）及其真实耗时、TTFT、成功与否、**费用**

这两样凑在一起，就构成了回放所需的全部输入：**"如果当时选了另一个候选，会
怎样"这个问题，只有把计划和结果放在一起才可能回答。** 只有结果没有计划，是
日志；只有计划没有结果，是意图。两者都在，才叫证据。

---

## 1. 必须打开的两个开关（这一步最关键，也最容易漏）

在配置文件里：

```toml
[ml_routing]
enabled = true
# 关键：必须显式指定。留空 = 没有任何持久化 ML 状态。
state_dir = "ml"
```

以及：

```toml
[shadow]
enabled = true
```

### 为什么 `shadow.enabled` 也必须开 —— 这是一个真实的隐藏依赖

`ml_routing.enabled = true` **不等于**能采集到数据。

样本的特征向量来自"决策时刻保留的输入"（代码里叫 decision-time input / 
`ShadowInput`），而那份输入是 **shadow 引擎评估时产出的**。`shadow.enabled = false`
时没有这份记录，于是：

| 计数器 | 值 | 含义 |
|---|---|---|
| `ingested` | 0 | 一条都没收 |
| `samples` | 0 | 样本池是空的 |
| `no_decision_time_input` | **等于你的总请求数** | 全部因缺决策输入被丢弃 |
| `traces_appended` | 0 | 一条 trace 都没写 |

我第一次跑实验时就是这个状态：`ml_routing.enabled = true`、接口全部正常、
promotion 接口返回正常 —— 但**一个样本都收不到，永远无法训练**。
`ml/dataset.rs` 的模块文档里写明了这一点，但 `ml_routing.enabled` 没有任何提示
它依赖 shadow。

**所以第一步不是发请求，是确认计数器在涨。**

### 为什么 `state_dir` 必须显式写

默认留空，而留空的语义是**"不启用任何持久化 ML 状态"**。这是有意为之：曾经尝试
过用操作系统推导默认目录，结果每个进程都打开同一个目录，互相污染，甚至测试会把
数据写进开发者的真实目录。

---

## 2. 确认数据在进来

服务起来之后，先看状态接口（只读，安全）：

```bash
curl -s -H "x-api-key: <你的 token>" \
  http://127.0.0.1:<port>/v1/ml/status | python -m json.tool
```

关注这几个字段：

```jsonc
{
  "status": {
    "durable_state": true,        // state_dir 生效了
    "traces_open": true,          // trace log 已打开
    "dataset": {
      "ingested": 0,              // 这两个必须开始涨
      "samples": 0,
      "no_decision_time_input": 0 // 必须保持 0
    },
    "traces": {                   // 这个必须开始涨
      "appended": 0,
      "nothing": 0,               // 持续增长说明有请求没产出样本，要查
      "refused": 0,
      "io_errors": 0
    }
  }
}
```

**判据：跑几十个请求后，`dataset.ingested`、`dataset.samples`、`traces.appended`
三者必须同步增长，且 `no_decision_time_input` 保持 0。**

任何一个不满足，这份数据就不可用于回放，请先解决再继续 —— 导出一份空的或残缺的
数据集，比不导出更糟，因为它会让人得出"这个场景不适合学习"的错误结论。

---

## 3. 采集期怎么跑

这一段决定了你最终数据的质量，比开关重要得多。

### 3.1 不要为了"好看"而调整真实使用

**不要**为了让数据更整齐去做以下任何一件事：

- 不要清洗、重排、筛选请求
- 不要只挑成功的请求
- 不要删除失败的记录
- 不要手工修正时间戳

失败和 fallback 恰恰是最有信息量的部分。回放要回答的是"另一个选择会不会更好"，
这只有在**当时那个更差的选择也被记录下来**时才可能回答。

### 3.2 单次 attempt 的样本量下限

先给一个已测到的经验值：**60 个请求不足以让门控通过，120 个可以**。

原因不是模型差，是门控要求成对证据（paired evidence）。当前默认门控要求
`min_paired_requests = 30`，而**探索会摧毁配对**（见第 6 节）。在探索为 0 的情况
下，60 请求的实测结果是 `BLOCKED`，120 请求是 `PROMOTED`，配对数分别是 2 和 91。

所以：**单个配置的请求数建议 120 以上**，越多越好。

### 3.3 覆盖到真实的 provider 切换

这是最容易被忽略、也最影响结论的一点：

> **如果你的某个 provider 从来没被尝试过，它在数据里就是不存在的。**

默认配置 `exploration_probability = 0.0`，此时探索在任何概率为 0 的情况下直接
返回"不探索"，连抽样都不做。于是一个静态计划永远不会选中的 provider：

- 收不到任何流量
- 因此没有观测记录
- 因此特征全是未知值
- 因此模型也没有理由选它

**这是一个闭环的死角。** 我在实验中亲眼见过：三个 provider，`charlie` 在第一阶段
之后**一次都没被路由到**，`blind_candidates=1`，它连"变成最优"的机会都没有。

所以采集期必须做到：

1. **每个在用的 provider 至少被真实尝试过足够多次**（建议 ≥ 60 次）
2. 如果你要评测"新增一个 provider 能否被发现"，那必须显式把
   `exploration_probability` 调到 0 以上 —— 并且接受随之而来的配对证据损失

### 3.4 别在采集期间改配置

配置文件在运行中被改动会造成同一段请求里候选集合不一致。中途改动请分段采集，
每段单独一个目录。

---

## 4. 导出

### 4.1 主数据：`traces.jsonl`

位置：

- Windows：`%APPDATA%\Zroutery\<state_dir>\traces.jsonl`
- macOS / Linux：`~/Library/Application Support/Zroutery/<state_dir>/traces.jsonl`
  （Linux 通常在 `~/.config/zroutery/<state_dir>/`）

文件名在源码里是 `ml::traces::TRACES_FILE_NAME = "traces.jsonl"`。

**直接复制这个文件即可，它是 JSONL，一行一条记录，不需要解压或转换。**

### 4.2 一条 trace 里有什么

```jsonc
{
  "schema_version": 1,
  "request_id": "...",              // 与其它日志的连接键
  "decision_id": "...",             // 同一个候选集合的决策 id
  "recorded_at": 1750000000,         // Unix 秒

  "input": {                         // ★ 当时"计划"了什么
    "decision_id": "...",
    "production_selected": "alpha-1",// 确定性计划选中的那个
    "is_fallback": false,
    "session_switch_count": 0,
    "feature_schema": 3,
    "task": { /* 任务画像：复杂度、任务类型等 */ },
    "candidates": [
      {
        "candidate_id": "alpha-1",
        "provider_id": "alpha",
        "tier": "standard",
        "eligible": true,
        "rejection_reason": null,
        "features": { "schema_version": 3, "values": [ /* 定长特征向量 */ ] }
      }
      // ... 每个候选一条，包括"当时不合格"的
    ]
  },

  "samples": [                       // ★ 实际"发生"了什么
    {
      "scope": { /* attempt 级或 request 级 */ },
      "attempt_id": "...",
      "targets": {
        "success": true,
        "latency_ms": 812.0,
        "ttft_ms": 143.0,
        "cost": 0.0003812,          // ★ 关键：每次 attempt 都有
        "failure_class": null,
        "fallback_count": 0
      }
      // 以及 features（与上面候选的特征一致，供训练直接使用）
    }
  ]
}
```

### 4.3 关键字段说明（回放时哪些字段真正被用到）

| 字段 | 在回放中的角色 | 注意 |
|---|---|---|
| `input.candidates[].features` | **决策时刻的特征**。没有它就无法重放"另一个选择" | 这是最容易被误认为可重算而丢掉的一份 |
| `input.production_selected` | 生产实际选中的候选 | 配对比较的基准 |
| `input.candidates[].eligible` | 资格过滤必须与当时一致，否则重放了当时不可能发生的动作 | |
| `samples[].targets.success` | 成功与否 | |
| `samples[].targets.latency_ms` / `ttft_ms` | 延迟维度的证据 | 失败时可能是 `null` |
| `samples[].targets.cost` | **成本维度的唯一来源** | 见下面的坑 |
| `samples[].targets.failure_class` | 失败分类，用于判断换 provider 是否可能有用 | 失败记录里这个很关键 |
| `attempt_id` / `scope` | 区分 attempt 级与 request 级样本 | 配对只在 request 级进行 |

### 4.4 顺便带上的两个文件（同目录）

| 文件 | 内容 | 用途 |
|---|---|---|
| `active-model.json` | 当前生效模型的指针、commit、完整门控决策 | 证明"服务时用的是哪个模型" |
| `active-model-audit.jsonl` | 每次晋升/回滚的记录 | 复盘时确认哪个模型在什么时间生效 |

只有你确实晋升过模型才会有后者。

### 4.5 不要做的事

- **不要调用 `POST /v1/ml/promote`**。那会在你的**生产环境**上真实安装一个模型。
  本文只涉及只读接口与文件复制。
- 不要把 `traces.jsonl` 提交进任何 git 仓库。
- 不要把 `features.values` 之外的原始 prompt/response 内容补充进来（本来也没有）。

---

## 5. 成本字段的坑（直接影响你的数据有没有用）

`targets.cost` **只在你的模型配置了 pricing 时才非空**，而且很容易小到在统计里
看不见。

### 定价的单位是"每百万 token"，不是"每 token"

这是我实测踩到的，而且**没有任何报错**。`Pricing::cost_of` 内部除以 `1_000_000`，
所以：

```toml
# 错：这是"每百万 token 0.0000005 美元"，比真实价格便宜约百万倍
input  = 0.0000005
output = 0.0000015

# 对：常见的真实量级（美元 / 百万 token）
input  = 0.15     # 便宜的模型
output = 0.6
```

写错单位的后果特别隐蔽：单价是百万分之一，报表按六位小数显示，**所有臂的
`mean_cost` 都是 `0.000000`**，成本维度等于零，而 utility 差异仍然只由成功率和
延迟决定 —— 看起来一切正常，只是成本这一维根本没参与。

**一个"每臂恒定"的指标，和"还没测"的指标在报表里长得一模一样。** 这是最需要
主动检查的地方。

采集期请确认 `cost` 有实际量级：

```bash
head -1 traces.jsonl | python -c "import sys,json;d=json.load(sys.stdin);print([s['targets']['cost'] for s in d['samples']])"
```

看到 `1e-10` 量级就是单位写错了。

### cache read 才是成本的大头，而且很容易漏配

在真实流量里，**cache read 的 token 数是新鲜 input 的 360–595 倍**，所以一次请求
的成本几乎全部是 cache read 成本。

Zroutery 的成本公式是：

```
成本 = input单价 × (input_tokens − cache_read − cache_write)
     + cache_read单价 × cache_read
     + cache_write单价 × cache_write
     + output单价 × output_tokens
```

两个容易踩的点：

1. **`Pricing::new` 不会设置 cache 单价**，`cache_read_per_mtok` 留空时
   **按 input 单价计费**。对一个 cache 占比 73% 的请求，这会把成本高估一个数量级，
   而且**各 provider 高估的倍数不同** —— 等于凭空造出一个价格表里并不存在的成本差异。
2. **cache 计价必须让 cache token 数占大头**，否则你测的是另一个维度。

### ⚠️ 与 CC Switch 数据的语义冲突（如果你两边都有数据）

`ir::Usage` 约定 `cache_read_tokens` 是 `input_tokens` 的**子集** —— 即
`input_tokens` 是**含 cache 的总 prompt**。

**CC Switch 的主流行用的是相反约定**：`input_token_semantics` 有三个取值，分布是
17836 / 4297 / 74，占 81% 的那批 cache_read 是 input 的 **595 倍**，也就是
`input_tokens` **不含** cache read。

后果很具体：`fresh_input_tokens()` 是饱和减法，`input − cache_read` 在这批数据上
直接变成 **0**，于是**新鲜 input 那一项被整项漏算**，成本系统性偏低。

所以跨工具比较成本之前必须先对齐这一条，而"对齐"**不能**理解为"按 Zroutery 的约定
处理"，因为多数派用的是另一套。正确做法是按 `input_token_semantics` 分组，两种
约定分别算，不要混在一起。

### 另外两件事

1. **不要把 pricing 表当作 ground truth 之外的第二个真值**。它是你自己的声明，不是
   账单核对结果。
2. **失败的 attempt 可能不产生费用记录**（上游若对 5xx 计费，这笔花费在数据里
   不可见）。如果你关心这块，需要在采集期明确知道它不可得，而不是把它当成 0。

---

## 6. 一个你必须自己知道的取舍：探索会毁掉晋升

这不是采集指南的问题，但它直接决定你采到的数据能不能用来评估"能否晋升模型"。

已实测的结论（探索概率 0.25 / 0.5）：

| 探索概率 | 60 请求 | 120 请求 | 240 请求 |
|---|---|---|---|
| 0.00 | BLOCKED | **PROMOTED**（配对 91） | **PROMOTED**（配对 181） |
| 0.25 | BLOCKED | BLOCKED（配对 2–15） | BLOCKED（配对 6–10） |

机制：门控要求"学到的模型"与"基线"在**同一批请求**上都有实测结果。探索把一部分
请求路由到了基线不会选的 provider 上，于是基线在这些请求上没有结果，配对数就塌了。
**流量翻倍也救不回来**，因为可配对的上限由探索概率决定，不由请求总数决定。

所以：

- 想要**能晋升的数据** → 采集期保持 `exploration_probability = 0`
- 想要**能发现新 provider 的数据** → 必须开探索，且接受门控判决不可复现

**这两个目前无法同时满足。** 这不是配置错误，是机制耦合。

而且要特别注意第二条的准确含义 —— 我实测发现，比"探索会摧毁配对"更麻烦的是
**判决本身变得不可复现**：

| 探索概率 | 配对数（第一次） | 配对数（第二次） |
|---|---|---|
| 0.00 | 91 / 91 | 91 / 91 |
| 0.02 | **6（BLOCKED）** / 73 | 53 / 53 |
| 0.10 | 38 / 40 | **10（BLOCKED）** / 75 |

探索为 0 时三次都是稳定的 91/91 晋升；探索大于 0 时，**每个设置我都拿到过
BLOCKED 和 PROMOTED 两种结果**。原因是配对取决于"学到的模型所提名的候选，
是否恰好在某个基线也被实测过的请求上被真正尝试过" —— 探索介入后这件事接近抛硬币。

**所以如果你要拿数据去评估"能否晋升模型"，保持探索为 0，并且一次运行的结果不能
当作结论** —— 请重复运行至少三次。

### 一个反直觉的补充：盲区比"不可达"窄

探索为 0 时，一个计划永不首选的 provider 确实拿不到**首选流量**，但它**仍然可能
作为 fallback 被走到**。我实测中 `charlie` 从未成为首选（`blind_candidates=1`），
但当 `bravo` 退化后，确定性 fallback 链走完了全程，`charlie` 拿到了 120/120 的请求。

所以"新加的 provider 永远拿不到流量"这个说法是错的。准确说法是：**拿不到首选流量，
兜底流量仍然有。**

---

## 7. 建议的采集流程

```
1. 配置 ml_routing.enabled = true
   配置 ml_routing.state_dir = "ml"
   配置 shadow.enabled = true        ← 不要漏
2. 启动，确认 /v1/ml/status 里 durable_state、traces_open 均为 true
3. 正常使用（不要刻意制造流量）
4. 每 50 个请求查一次 status，确认 ingested / samples / traces.appended 在涨，
   且 no_decision_time_input 保持 0
5. 正常运行若干天（单个配置建议 ≥ 120 请求，最好上千）
6. 停机后复制 state_dir 整个目录
7. 记录 metadata（见下）
```

### 随数据一起交回这些信息

这些是数据本身不含、但没有它们就无法正确解读的东西：

- Zroutery 版本 / commit（`git rev-parse HEAD`）
- 配置文件全文（**请自行删除其中的密钥、token、cookie**）
- provider 列表与各自的 `priority`、`tier`、`enabled`
- 每个 model 的 pricing 配置
- 采集起止时间
- 采集期间是否发生过配置变更、provider 增删、重启
- 当时 `ml_routing.exploration_probability` 的值
- 是否曾安装过模型（若有，`active-model.json` 一并带上）

### 明确标注为 `null` 的字段

任何你无法确认的字段，请直接写 `null` 或 `"unknown"`，**不要猜、不要填默认值**。
一个诚实的 `null` 是可用的；一个猜出来的数字会让整份数据不可信。

特别地：

- 价格表**不是**账单的 ground truth
- `observed_model`（如果你还有别的工具链的日志）是**观测**，不是 Zroutery 的
  **动作**。某次请求实际由哪个后端应答，不等于路由层当时可以选择它。

---

## 8. 一页速查

| 要做的事 | 怎么做 |
|---|---|
| 打开采集 | `ml_routing.enabled=true` + `ml_routing.state_dir="ml"` + `shadow.enabled=true` |
| 确认在采集 | `GET /v1/ml/status` 看 `ingested`/`samples`/`traces.appended` 在涨，`no_decision_time_input=0` |
| 导出 | 复制整个 `state_dir` 目录 |
| 主文件 | `traces.jsonl`（JSONL，逐行） |
| 附带文件 | `active-model.json`、`active-model-audit.jsonl`（若有） |
| 最低请求量 | 单配置 ≥ 120（60 实测不足） |
| 最容易漏的坑 | `shadow.enabled` 没开 → 收不到任何样本 |
| 成本维度 | 必须配 pricing，**单位是每百万 token**，且必须显式配 cache read 单价 |
| 成本自检 | `targets.cost` 应在 `1e-3` 量级；出现 `1e-10` 就是单位写错了 |
| 跨工具注意 | CC Switch 主流行的 `input_tokens` **不含** cache read，与 Zroutery 约定相反，直接算会漏掉新鲜 input 项 |
| **不要做** | 调用 `POST /v1/ml/promote`（会在生产上真实装模型） |
| 已知不可兼得 | 「能稳定晋升」与「能发现新 provider」目前互斥；探索 > 0 时门控判决不可复现，需重复运行 ≥ 3 次 |

---

## 9. 回传后我们会做什么

拿到数据后的顺序是：

1. **只读导入 + 校验**（原始记录保持可回溯，字段不足一律 `null`）
2. **回放**：在**同一批请求序列**上跑 ML 与全部基线（`Priority` / `RoundRobin` /
   `LowestLatency` / `Balanced`），比较成功率、fallback 率、延迟与 P95、TTFT、
   token、成本、utility、regret、provider 切换次数、约束违规数
3. **只汇报观测到的差异**，不预设结论。若 ML 没赢过某个基线，如实写出来

需要你额外提供的一件事：**一段你认为"Zroutery 明显选错了"的真实请求区间**
（时间范围即可，不需要内容）。它是我们验证回放管线是否可信的标尺——如果回放没能
重现出你已经知道的那次错误选择，那么回放本身就没有被信任的理由。