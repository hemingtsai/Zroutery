# Zroutery 安全插件 API 设计 v1

> 状态：设计草案，待用户审阅；2026-11-13。本文中的 API、路径和命令是拟议合同，尚未实现。
> 源码基线：`dev` / `26d887fd4733fba374a769db2e50653ad98d114b`，开始调研时工作区与暂存区为空。
> 可供类型检查的接口草案：[plugin-api-v1.d.ts](plugin-api-v1.d.ts)。
> 编译期边界断言：[plugin-api-contract.test-d.ts](plugin-api-contract.test-d.ts)。
> 用户补充约束已纳入：request/response 等复用既有 Rust 基底，不进入 JS 沙箱；反代应用 Token 使用同一 keyring；不能读取其他凭据；插件权限逐项显式声明。
> Rust JavaScript 绑定选定 **rquickjs**，底层为 QuickJS-NG；这里的 QuickJS 表示引擎家族，具体宿主集成采用 rquickjs。

## 1. 结论

采用 **Rust capability broker + 独立 OS 沙箱 worker + rquickjs / QuickJS-NG**。
插件提供服务商/客户端/传输配置声明与 UI 描述。**请求、响应、协议转换、流式处理、认证交换、
实际网络发送、重试、计量及唯一结算全部复用 Rust Core。QuickJS 只运行控制面和 UI 逻辑。**

用户要求对应四类扩展点：

1. **Provider / Account**：添加服务商、订阅或特定客户端后端，组合现有 Rust 协议、认证、目录、额度与费率服务。
2. **Client / Transport**：声明客户端接管、已有入口适配器、受约束的 TLS / HTTP 指纹 profile，由宿主执行。
3. **UI**：用宿主组件组成扩展面板、页面、表单和动作；QuickJS 只产出数据，React 负责渲染和交互。
4. **Theme**：MD3、Fluent 迁移为随应用分发的数据型插件；Industrial 为内建默认及恢复界面。

数据面不调用 JS：没有 `onRequest`、`prepare`、`decode`、body handle、原始响应、通用 fetch 或明文 secret API，
也没有可申请的 request-read 权限。JS 崩溃不影响已启用的 Rust 数据面；禁用/撤销由 Rust 策略独立生效。
反代所需应用 Token、OAuth bundle、session cookie 均由现有 keyring 保存，JS 只能在明确授权后看到自己槽位的状态。
上游 endpoint 会收到用户路由给它的内容，因此插件声明的目的地与凭据用途必须在启用前清楚可见。

这是以现有 Core 为中心的扩展机制。日常入口仍是“服务商 → 模型与路由 → 客户端接入 → 用量与记录”，
插件管理放在设置中；安装包存在、插件启用、账户验证及可路由状态分别表示。

## 2. 源码依据与 Magpie 取舍

### 2.1 Zroutery 当前接缝

| 现有位置 | 已有能力 | 本设计接入点 |
| --- | --- | --- |
| `crates/zroutery-core/src/config/{provider,registry}.rs` | `ProviderKind`、API surface、客户端 profile、账户/Key 声明 | 增加版本化 adapter binding，插件 ID 与协议类型分别表达 |
| `crates/zroutery-core/src/upstream/{http,headers,stream,observation}.rs` | reqwest 池、直连/系统代理、HTTP 指纹、流和原始用量观察 | 暴露受控 Rust service binding 与 `TransportPort`，请求不经 JS/IPC |
| `crates/zroutery-core/src/upstream/http.rs` | 当前 HTTP/1.1、禁重定向，90 秒空闲池 | 新 profile 是逐实例独立池，原传输策略仍是 native 默认 |
| `crates/zroutery-core/src/server/pipeline/lifecycle.rs` | 请求/尝试身份、已知用量、唯一终态及持久结算 | 插件只提交提案，不能调用结算或改写观察事实 |
| `crates/zroutery-core/src/account/`、`billing/`、`runtime_store/` | 账户代次、观测、报价及账目各自有所有者 | 复用，不创建插件版账户/预算账本 |
| `src-tauri/src/secrets.rs`、`secret_refs.rs`，Core `config/secrets.rs` | `KeychainSecrets / KeyringBackend / SecretStore`、缓存及引用清理 | 包一层 scope 校验；复用现有 keyring，不直接把通用 `get(key_ref)` 暴露给插件 |
| `crates/zroutery-core/src/agent_takeover/`、`file_transaction.rs` | 受管字段、预览、指纹 CAS、恢复 | 插件绑定宿主 schema/受管 slot，提案和写入均由既有 Rust 层生成 |
| `ui/src/skin.ts`、`theme.ts`、`accent.ts` | Industrial / MD3 / Fluent，风格与亮暗两轴、系统 accent | 风格选项改为注册表；亮暗轴独立 |
| `ui/src/styles.css` | 四份 MD3/Fluent 亮暗 token 表及大量 layer-3 组件规则 | 搬迁完整组件配方，不能退化为只换色板 |
| `src-tauri/{build.rs,capabilities/}`、`src-tauri/src/lib.rs` | 显式命令注册/ACL，main 与 tray 权限边界 | 管理命令走最小 ACL；插件 JS 从不进入 dashboard JS realm |

约束以 [upgrade-contracts.md](upgrade-contracts.md) 及其引用的已接受总计划为准。
主题以 [skin-spec-md3-fluent.md](skin-spec-md3-fluent.md) 的后续裁决和当前源码为准：MD3 控件 36px，
Fluent 32px，Industrial 30px；不能重新套用文档早期的“全部 30px”。当前源码还包含后续颜色调整，迁移前须逐值冻结。

### 2.2 Magpie 不是单一的插件权限模型

本地参考目录 `E:/zr-research/magpie` 为 `a6951cc63af6b992625a96d93aafe438c14ffab5`。
另外阅读了 2026-11-13 的官方插件文档，以及固定到
`92bce5c724bcc857474a7ba5c135f0d36efecd31` 的 `internal/pluginrt/pluginrt.go`、
`internal/provider/plugins.go`。以下是源码/文档比较，不是对其产品做过安全测试的结论。

| Magpie 机制 | 借鉴 | Zroutery 调整 |
| --- | --- | --- |
| 内建 `Manifest / Requires / Provides / Parts / OnDispose` | 能力发现、依赖排序、贡献点、反向清理、局部失败 | 依赖是版本化宿主服务；注册到 staging，成功后原子发布；v1 无跨插件对象/函数共享 |
| Provider 的 config/auth/models/usage/refresh hooks，统一账户与 failover | 服务商进入统一目录、账户生命周期和请求链 | 显式 descriptor 绑定现有 Rust 服务；请求/认证/目录解码均留在 Rust |
| 一个 Bun host 装所有 provider，提供 shell、文件及原生 fetch | 包结构、开发体验、订阅接入形态 | 每插件/权限域/账户隔离；使用 QuickJS，分发 ESM，宿主不运行包管理器 |
| moejs 中间件按请求建 runtime、仅包内 import | 包内 import、配额、局部诊断 | JS 仅运行控制/UI；本设计不引入请求中间件或逐 token JS 调用 |
| HTML 页面以 sandbox iframe + 受限 bridge 嵌入 | slot、局部出错、按读权限的投影 | v1 用声明式 UI，避免 DOM/脚本与导航外传；不把 iframe 当完整秘密隔离 |
| 内建功能插件化及 Requires/Provides | 可关闭扩展、核心只认接口 | 先迁 MD3/Fluent；路由/预算/结算等生产权威留在 Core |
| 部分社区包自动更新，冲突 provider 有选择规则 | 插件版本可见、替换可恢复 | 固定内容摘要，更新由用户触发；冲突显式选 owner，不依安装顺序覆盖 |

Magpie 文档明确区分 Bun 的宿主权限、moejs 的无 Node/网络环境、页面的 sandbox。
因此这里的“更安全”是**缩小动态 provider 的权限、数据共享范围和故障范围，并统一 broker 校验**，
不是把 Magpie 的所有插件都描述成无隔离。

## 3. 架构及依赖方向

```text
控制面：manifest / Tauri UI / headless CLI
             ↕ 权限校验、有限动作与非秘密投影
        Rust plugin manager / broker ←有界 IPC→ QuickJS 沙箱（control / ui）
             │ 原子发布已验证的声明与 grant epoch
             ▼
数据面：客户端 → 既有 Rust Core → 既有 keyring + Rust transport → 上游
                  协议 / 路由 / 账户 / 重试 / 流 / 计量 / 结算
                  （整个请求生命周期不调用 QuickJS）
```

- Core 的领域类型和 eligibility 仍是权威，不能依赖某个插件才能做基本路由。
- 控制/UI port 接收不带秘密、正文或原始响应的版本化 DTO，返回可校验表单/动作提案。
  不能把 `AppState`、请求/流对象、数据库连接、`reqwest::Client`、`SecretStore` 或 Tauri `AppHandle` 暴露给 JS。
- `zroutery-plugin-api`：无 GUI/网络/QuickJS 依赖的 DTO、schema、错误码、IPC 版本。
- `zroutery-plugin-host`：安装、策略、supervisor、RPC、资源与撤销；依赖 API，不依赖 Core 内部类型。
- `zroutery-plugin-worker`：仅 QuickJS、DTO/IPC 与平台沙箱入口；不链接凭据库、Tauri 或通用出站客户端。
- Core 的 `plugin` feature 提供声明绑定与 Rust grant 检查，桌面/headless 在 composition 层注入 broker。
  账户类宿主服务要求 `account`，普通 API-key provider 不强制启用账户、NewAPI 或 ML。
- 数据型 provider/client/theme 不需要 JS entry；带扩展表单的包才启动 QuickJS。
  已验证的声明由 Rust registry 持有，UI/control worker 不在请求关键路径上。
- 同一插件的 `control`、`ui` 与 `host_binding` 权限**不合并**；后者是 Rust 代办操作的授权对象，
  不是拥有网络/凭据 API 的 JS facet。纯代码可以复用，运行态和 authority 分离。

## 4. 权限、威胁模型与数据边界

### 4.1 实际权限如何计算

```text
effective = 宿主硬边界
          ∩ 包声明权限
          ∩ 用户对此安装版本/绑定的授权
          ∩ 当前 principal / 宿主服务 / provider / account / 操作的作用域
```

**每一项权限必须在 manifest 显式声明，即使是官方插件、数据型 provider 或内置主题包。**
`PermissionRequest` 不是万能的 `network: true`。每条记录绑定插件稳定 ID、包摘要、principal、
contribution ID、服务商实例、账户代次、允许 operation/origin/credential slot/profile 和 grant epoch。
声明是申请，不等于授权；UI 必须展示所需能力，用户批准后才能启用对应能力。主题选择可以完成纯外观授权，
无需额外重复弹窗，但声明与授权记录仍存在。缺声明的贡献拒绝加载；不得由 SDK、hook 名或插件类别推导隐式权限。
缺省拒绝；未知必需能力使对应贡献不可用，未知可选能力保持未授权。签名、更新、依赖不提升权限。

| 能力 | 可做的事 | 不随之获得的权力 |
| --- | --- | --- |
| `provider.register` | 提供本命名空间 descriptor，用户实例化 | 覆盖既有 provider、自动启用路由 |
| `host.operation.bind` | 把命名 Rust 服务绑定到明确 origin/operation，由 Core 按正常任务触发 | JS fetch、动态 HTTP 请求、请求/响应原文 |
| `credential.store` | 宿主密钥录入/认证流程保存本插件声明的槽位 | JS 得到输入值、覆盖他人凭据、任意 key_ref |
| `credential.use` | Rust 在绑定操作中使用本插件实例/账户槽位 | 读取/导出明文，读取其他插件或原有 provider 的 Key |
| `credential.status / refresh / clear` | 各自独立授权自己的状态查询、宿主刷新、用户触发清除 | 枚举 keyring、跨账户状态/刷新/删除，自动继承兄弟 slot 权限 |
| `client.register / setup.propose / setup.restore` | 分别绑定已有 Rust 入口/接管服务、生成预览、请求恢复 | JS 协议解码、免鉴权、任意文件或 shell |
| `transport.profile.use / declare` | 使用或声明有限 TLS/HTTP profile | 关闭证书验证、任意 socket、上传 DLL 或修改全局代理 |
| `ui.contribute / ui.projection.read` | 固定 slot 的宿主组件和明确实体的投影 | DOM、Tauri invoke、任意 HTML/JS/CSS、读取凭据/正文 |
| `host.action.request` | 真实用户动作选择 manifest 列出的有限宿主操作 | JS 自行定时发起操作、跨实体目标、绕过该操作本身所需授权 |
| `options.read / update` | 各自独立读取自己的非秘密设置、提议修改 | 全局 config、其他插件选项、改服务商 endpoint/权限 |
| `diagnostic.report` | 上报 manifest 声明的诊断码 | 自由文本日志、原始错误/请求/凭据 |
| `theme.contribute` | 颜色/几何/字体角色及有限组件配方 | 网络、账户、覆盖安全确认和恢复界面 |

JS 不持有凭据 handle，只使用 manifest-local slot 名和宿主提供的安全状态。实际 keyring reference 仅由 Rust 解析。
UI action handle 是不可猜宿主表项，绑定安装/进程代次、facet、实体/账户版本、action、到期和 grant epoch。
每次 RPC 重新校验，不能信任插件自报 plugin_id/account_id。跨账户、worker 重启后或禁用后的迟到结果拒绝。
TypeScript branded type 只防编程错误。未授权与不存在的其他槽位返回同一拒绝类别，避免存在性枚举。

作用域规则是合同的一部分：`operations/origins/credential_slots/profiles/projections/actions` 按能力分别必填，
空数组、通配符、未声明 ID、跨 contribution 引用都不能表示“全部”。允许字段是封闭集合，
不相关字段同样拒绝，避免作者误以为某个无效限制被执行。`contributions: ["$plugin"]` 只用于
本包 options/诊断；provider/client/theme/action 则引用各自的实际贡献 ID。
不能把两条不完整权限交叉拼成新权限：一次 credential use 必须存在一条同时覆盖 owner、slot、operation、origin 的 grant。

### 4.2 JS 边界只传非秘密控制数据

- v1 无插件任意文件系统，也无任意持久 KV API。配置由用户表单生成，观测由 Core 的类型化 store 接收。
  control/UI 只拿自己获准的非秘密 options，token 字段一律由宿主密钥控件和 keyring 持有。
- 生产 `report(code)` 只接收包内预声明的诊断码；宿主附上耗时、字节数和错误类别。
  JS 异常 message、stack 源码片段、console 内容、IPC 原文和完整 URL query 不写入生产日志。
  stdout/stderr 有界丢弃或仅保留宿主生成的退出码，不能被插件用作第二日志通道。
- 调试器只能在隔离临时根与合成数据模式显式启用。崩溃转储默认关闭/禁止自动上传；OS 管理员级转储不属于插件沙箱可保证的范围。
- 模型名、资源名、i18n、UI 文本等是被限定投影的非秘密数据，长度/控制字符/结构校验并按文本渲染。
  不投影原始网络错误、auth 响应、请求 metadata、session ID、工具参数、prompt 或 response。
- `host.operation.bind` 的 origin/协议/profile 是安装或明确编辑时批准的静态声明，不能由请求内容、
  JS callback、UI 任意字段或 options migration 改写。上游 destination 仍是数据接收方，域名权限要清楚显示。
- 宿主服务解析认证、目录、额度和费用；UI 只能读相应类型化结果。上游回显凭据时也不能把 auth payload
  透传到 JS；已知 secret 字段/值额外过滤，错误转结构化 code。
- 沙箱限制恶意代码的 OS 能力；keyring 隔离则在 Rust capability 层逐次校验，不能仅依赖 JS 名称隐藏或“请勿访问”。

## 5. 包格式与公开 API

安装工件为 `.zrp`（受限 ZIP）或开发目录的不可变快照，内容示例：

```text
acme-provider/
  zroutery.plugin.json
  dist/control.mjs
  dist/ui.mjs
  schemas/options.json
  locales/en.json
  locales/zh-CN.json
  locales/zh-TW.json
  LICENSE
```

安装器只验证、解包、归档。作者在开发机用 TypeScript 和 bundler 产出 ESM；客户端不安装 npm/Bun，
不运行 install/postinstall，不从网络动态 import。`zroutery:plugin/v1` 是宿主模块，
相对 import 只能命中摘要覆盖的包内 ESM。Node/Bun、`std`/`os`、`file:`、网络 URL、原生模块加载均不可用。

工件签名放在独立 envelope：插件 ID、发布者公钥、版本、兼容 API、规范化文件列表/摘要一起覆盖；
hash 证明内容，签名证明发布者，均不自动提升权限。用户本地未签名包可以进入“本地快照”安装流程，权限模型相同。

示例：数据型 provider 复用 Rust Chat 服务，应用 Token 使用现有 keyring。
下面的 `host.*` 名称是拟议 service registry ID，不声称当前已注册。所有必需能力均显式列出：

```json
{
  "manifest_version": 1,
  "id": "dev.acme.coding",
  "version": "0.1.0",
  "name": { "en": "Acme Coding", "zh-CN": "Acme 编程", "zh-TW": "Acme 程式設計" },
  "host_api": { "major": 1, "min_minor": 0 },
  "runtime": null,
  "requires": [
    { "service": "host.protocol.openai_chat", "api": { "major": 1, "min_minor": 0 } },
    { "service": "host.auth.bearer", "api": { "major": 1, "min_minor": 0 } }
  ],
  "permissions": [
    {
      "capability": "provider.register",
      "principal": "host_binding",
      "contributions": ["coding"],
      "reason": { "en": "Add the Acme provider", "zh-CN": "添加 Acme 服务商", "zh-TW": "新增 Acme 供應商" }
    },
    {
      "capability": "host.operation.bind",
      "principal": "host_binding",
      "contributions": ["coding"],
      "operations": ["completion"],
      "origins": ["https://api.acme.example"],
      "reason": { "en": "Use the host Chat service at Acme", "zh-CN": "通过宿主 Chat 服务连接 Acme", "zh-TW": "透過宿主 Chat 服務連接 Acme" }
    },
    {
      "capability": "credential.store",
      "principal": "host_binding",
      "contributions": ["coding"],
      "credential_slots": ["app_token"],
      "reason": { "en": "Store this connection's app token in the system keyring", "zh-CN": "在系统 keyring 保存此连接的应用 Token", "zh-TW": "在系統 keyring 儲存此連接的應用 Token" }
    },
    {
      "capability": "credential.use",
      "principal": "host_binding",
      "contributions": ["coding"],
      "credential_slots": ["app_token"],
      "operations": ["completion"],
      "origins": ["https://api.acme.example"],
      "reason": { "en": "Let Rust authenticate this connection", "zh-CN": "由 Rust 使用此连接的 Token 认证", "zh-TW": "由 Rust 使用此連接的 Token 驗證" }
    }
  ],
  "contributions": {
    "providers": [{
      "id": "coding",
      "name": { "en": "Acme", "zh-CN": "Acme", "zh-TW": "Acme" },
      "operations": [{
        "id": "completion",
        "service": { "id": "host.protocol.openai_chat", "api": { "major": 1, "min_minor": 0 } },
        "base_url": "https://api.acme.example/v1",
        "credential_slots": ["app_token"]
      }],
      "credentials": [{
        "id": "app_token",
        "name": { "en": "App token", "zh-CN": "应用 Token", "zh-TW": "應用 Token" },
        "kind": "app_token",
        "scope": "provider_instance",
        "storage": "host_keyring",
        "acquisition": { "kind": "host_secret_form" },
        "auth_service": { "id": "host.auth.bearer", "api": { "major": 1, "min_minor": 0 } }
      }]
    }]
  }
}
```

这个包没有请求脚本，也不需要启动 JS。Key 在宿主密钥表单录入、按新实例归属写入 keyring，
Rust 把正常 Core 请求发到获准 endpoint。插件不能指定既有 `provider:...` 引用或列出其他 Key。
目录/额度/OAuth/TLS profile 或自定义 UI 均要再声明对应贡献和权限，不能由上述四项自动带出。

如需自定义 UI，追加 `runtime.entries.ui` 与 `ui.contribute`、特定 `ui.projection.read`。
以下示例只把 Rust 已生成的安全状态投影画成宿主组件：

```js
import { define_plugin } from "zroutery:plugin/v1";

export default define_plugin({
  ui_render: {
    connection(input) {
      return {
        kind: "value",
        label: "连接状态",
        value: typeof input.projection.status === "string"
          ? input.projection.status
          : "未知"
      };
    }
  }
});
```

正式插件按 `input.locale` 提供英/简中/繁中；增加按钮时，action ID 必须绑定 manifest 内的
`HostActionBinding`，且有 `host.action.request` 与底层操作权限。JS 只能返回该动作的提案，不能执行任意 HTTP 请求。

### 版本与 schema

- host API major/minor、插件版本、包内容摘要、协议 adapter 版本、options schema、theme schema 分别存储。
- v1 DTO 为 snake_case；金额/高精度 token 数使用十进制字符串，拒绝负数、NaN、无穷、非规范数字。
- 数据必须经过有深度/字节/集合长度上限的严格 decoder；拒绝 duplicate key、`__proto__` 等危险键，
  不触发插件提供的 getter/toJSON 做不受控宿主工作。JS 内序列化也计入执行预算。
- API major 不匹配或缺必需能力即该贡献不可用；新可选字段可忽略，但不能靠忽略未知权限扩大授权。
- SDK 不暴露 Core 的 `ChatRequest`、`ChatResponse`、`StreamEvent` 或其 handle。
  `HostServiceRef` 只引用宿主已实现的 Rust 服务，作用/网络/计量类别由 registry 决定，不能被插件自报成“只读”。
  `.d.ts` 中 service_options、projection、role/part 的具体 schema 在 P0 冻结；字符串/JsonObject 不意味着任意对象获准。

### 只有四类 JS 调用

| JS hook | 输入/输出 | 必需权限与调用条件 |
| --- | --- | --- |
| `validate_options` | 本包非秘密 options → 字段错误码 | `control: options.read`，只能增加校验失败，不能覆盖宿主 schema |
| `migrate_options` | 旧版非秘密 options → 新版副本 | `control: options.read + options.update`，宿主更新事务触发 |
| `ui_render` | 指定作用域安全投影 → UiNode | `ui.contribute`，读取任何投影另要 `ui.projection.read`，读槽位状态还要 `credential.status` |
| `ui_action` | 宿主绑定的真实动作与非秘密字段 → 单个 UiEffect | `host.action.request`，并满足 manifest action 的底层权限 |

IPC 使用宿主创建并限制对端的匿名 pipe/平台等效私有通道，不监听本机 HTTP 端口。
帧带长度前缀、`ipc_version / call_id / worker_generation / grant_epoch`，反序列化先查大小和嵌套上限。
插件身份/账户视图来自通道和在途调用表，不从 worker JSON 取；回包必须匹配 method/result 类型和当前 revision，
同 call 只能完成一次。`report` 作为单独有界诊断码帧处理，不能在 result 上附任意可记录文本。
取消时作废 call/action，必要时杀 worker；排队或迟到回复不能新建宿主操作。这里的 IPC 没有 HTTP 请求/流消息类型。

## 6. QuickJS 运行时与 OS 沙箱

### 6.1 选定 rquickjs

调研时 `rquickjs = 0.14.0`，crate 声明 Rust 1.87，实际引擎为 **QuickJS-NG**；符合用户的 QuickJS Rust 绑定方向。
选择依据：带生命周期约束的 Rust 值封装、可控 ESM resolver/loader、显式 Runtime/Context、
内存/栈/interrupt 接口以及按需启用的 Rust Future/JS Promise 互操作；适合按能力封闭的嵌入式 API。
控制/UI hooks 首版保持同步，按需使用 Promise job 驱动，不因库支持异步就开放任意 I/O 或逐请求回调。
仓库最低 Rust 为 1.90；仍需用锁定全依赖图验证，单 crate 的 rust-version 不是构建证明。
QuickJS-NG 是 C 引擎；Rust 安全绑定、内存限额和缺省无宿主 API 都不等于 OS 安全边界，所以可执行扩展放在受限进程。
该版 README 对 Windows MSVC 的引擎支持仍标 experimental；有预生成绑定不代表 Windows native 已验收，P0 必须实测。

拟议依赖：

```toml
rquickjs = { version = "=0.14.0", default-features = false, features = ["std", "loader"] }
```

用专用 worker 线程驱动 Runtime 与 Promise job queue，RPC promise 由 worker dispatcher 兑现。
如实现确需 `futures` 再单独启用；它不改变 authority。不开 `full / full-async / dyn-load / parallel`。
其中 `full` 明确包含 `dyn-load`；`std` 是 Rust 标准库 feature，不代表为 JS 注册 QuickJS 的 `std`/`os` 模块。

- `set_memory_limit` + `set_max_stack_size` + `set_interrupt_handler` 同时使用。
- 0.14.0 文档提示使用自定义 allocator / `rust-alloc` 时 `set_memory_limit` 不生效。
  因此默认保留其受限 allocator；任何 allocator 变更必须补等效字节配额与 OOM 实证。
- interrupt 是周期性协作检查，不是精确 fuel。外部 `tokio::time::timeout` 不能抢占同步 JS，
  host callback 不能阻塞、不持 Core 锁调用 JS；最终由独立 supervisor 强制结束卡住的 worker。
- 新建 Runtime 才是新 heap；同 Runtime 的多个 Context 可共享对象，不作为插件/账户隔离。
- 只加载源码 ESM。第三方 QuickJS bytecode 不可信且不跨版本稳定；v1 不接受。
  可做 **worker 内**版本绑定的内存编译缓存；外部持久 bytecode/VM 快照不作为插件输入。

### 6.2 隔离单位

- 一个 worker process 只装一个包摘要的一个 JS facet 和一个授权投影域；不同插件/账户投影不共进程。
  不需要账户信息的 control/UI 在无账户域运行，不给所有账户隐式通配权。
- 每次配置验证、迁移、UI render/action 使用短生命周期 Runtime；需要有状态 UI 时把非秘密状态显式放进
  受 schema 约束的宿主 view state。不能用 module globals 跨实体保留旧投影。数据面没有 request Runtime。
- 懒启动 worker，空闲回收；全局/单插件 worker 数、并发调用、等候队列与总内存有上限。
  配额满即排队超时或 `plugin_busy`，不能退回主进程执行，也不能借“扩容”合并权限域。
- 初始化模块时没有网络/凭据/用户数据。注册由静态 manifest 与纯 hooks 完成，加载失败不产生部分贡献。

### 6.3 跨平台限制必须由 OS 落地

| 平台 | worker 约束 | 接受条件 |
| --- | --- | --- |
| Windows | 无网络 capability 的 AppContainer/受限进程，Job Object 内存/进程数/终止、禁止子进程；仅继承显式 IPC handle | 实际验证不能开 socket、读主配置/凭据、打开父进程内存或逃逸新子进程。Job Object 单独不构成安全沙箱 |
| macOS | 签名的沙箱 helper，受约束的 IPC、只读必要资源、无网络/用户目录权限及 keychain access group | macOS 11+ 对应架构、签名/entitlements、子进程与父进程访问负例；不能把已弃用 sandbox-exec 当长期发行方案 |
| Linux | user/mount/network namespace、no_new_privs、seccomp，必要时 Landlock；只读最小运行根、隔离进程访问 | 独立 build/runtime 记录；用户命名空间不可用时返回 sandbox_unavailable，不能静默退到同 UID 普通进程 |

平台能力不足时该平台仍可运行通过验证的数据型 provider/client/theme；可执行控制/UI 扩展显示不可用原因。
第三方 ESM 必须在 OS 沙箱建立后才读取/执行，包字节经 broker 传入；清空继承环境（尤其 headless Key 环境变量）、
设置空工作根、固定 worker 路径、不经 shell 搜 PATH。验证父进程/同用户文件访问限制，不能仅验证 JS API 不存在。

### 6.4 初始资源预算（工程起点，不是性能实测）

| 范围 | 建议默认上限 |
| --- | --- |
| control/UI Runtime | 16 MiB JS heap、512 KiB JS stack；worker 总内存 64 MiB |
| supervisor 全局 | 8 个存活 worker、512 MiB 总配额；每插件最多 4 个并发调用、等待 32 项 |
| 模块初始化 | 1 秒 wall，独立冷启动上限 5 秒 |
| 配置验证/迁移 | 50 ms 活跃 JS、200 ms wall；迁移经用户触发可使用独立有限档 |
| UI render/action | 50 ms 活跃 JS、200 ms wall，至多 500 节点/深度 16 |
| RPC | 单帧 256 KiB；单调用总 IPC 1 MiB；投影/输出队列有界，数据流不进 IPC |
| host calls | 单调用默认 16 次；实际宿主动作还要验证 action grant/真实用户意图 |

heap 之外的 Rust buffers、IPC 队列、UI 投影、临时 module 输入都计入独立 byte budget。
worker 不能以 Promise/microtask 无限续命。输入 schema 禁止 Request/Response/raw bytes/secret DTO，
不是收到大正文后再截断或脱敏；授权查询只从 Rust 领域投影构造。
长响应、媒体和大上下文保留现有 Core 的超时、背压、内存及结算机制，其执行与 JS 资源预算互不阻塞。

## 7. 出站、凭据和 TLS 指纹

### 7.1 操作级出站，而非通用 fetch

插件声明 `HostOperationBinding`：命名 Rust 服务/API 版本、base URL、credential slots、有限 service_options。
HTTP 方法、路径模板、参数 schema、响应解析、调用用途/副作用和计费类别由 **宿主服务注册表**确定，
插件不能把推理接口声明成免费 catalog 或通过自由表达式动态生成请求。
实例化时用户批准具体 origin；base URL 的后续编辑触发权限/凭据绑定重新校验。
相同 origin 也不能让 catalog/额度操作使用推理路径，凭据 authority 绑定 operation。

Core 从正常客户端请求、用户动作、已配置账户调度触发既有 service。认证交换、目录/额度/费率
请求和响应解码也在 Rust 中完成，JS 只接收获准的安全结果投影。新私有协议复用既有 Rust adapter 扩展边界，
以应用版本发布；宿主服务不存在即该 provider 不可用，不用 JS 回退实现 HTTP/协议引擎。
多步骤会话继续由 Core 的有界状态机驱动，每个外部副作用/可计费动作有身份和重试规则。

broker 统一实施：

1. 规范化 URL，检查 scheme、主机、端口、路径模板和 query 参数；拒绝 userinfo、控制字符、
   绝对路径替换 origin、混淆 IP/IPv6 zone、转义后越界和不支持 scheme。
2. DNS 解析后验证所有候选 IP 并锁定连接目标，重新解析/重试时重新校验，保留正确 TLS SNI/证书主机名。
   默认阻止回环、私网、link-local、云 metadata、组播及特殊地址；本地模型须用户单独授予确切 endpoint，不能授权整个私网。
3. 代理选项由宿主配置决定。若 proxy 远端 DNS 使 broker 无法保证目标 IP 分类，必须采用可验证的
   CONNECT-to-pinned-IP + 原 SNI，或已纳入信任边界的策略代理；无法提供保证的组合报不支持。
   不允许检查一次域名后交给任意代理再解析，声称已防 DNS rebinding。
4. 默认不跟重定向；需要时只按批准的同-origin 规则重新验证每跳，凭据不跨 origin，重放 POST 必须过 Core 重试判定。
   auth 重定向由 OAuth recipe 按 issuer/redirect URI 规则处理。
5. auth headers/cookies、Host、Content-Length、Transfer-Encoding、Connection 等由宿主管理。
   非敏感 header/指纹参数由服务或 profile 的静态 schema 白名单校验；不支持 JS header hook，
   不接受凭据硬编码进 service_options。CRLF/重复 framing 拒绝，凭据最后注入。
6. 限制压缩前/后大小、响应头、帧尺寸、并发和空闲时间。禁止代理回自身网关形成递归，保留本地例外的显式作用域。

### 7.2 同一 keyring，按插件归属隔离

**复用 `src-tauri/src/secrets.rs` 的 `KeychainSecrets → KeyringBackend` 和 Core `SecretStore`。**
Windows Credential Manager、macOS Keychain、Linux Secret Service 沿用现有 target-specific keyring 后端，
使用同一应用 service `app.zroutery.desktop`；不创建 `plugin-auth.json`、插件密码文件或另一套加密数据库。

持久 ownership binding 只含非秘密元数据：

```text
插件安装身份 + plugin_id + provider_instance + account_generation + slot_kind
  + slot_id + credential_revision → host_generated_key_ref
```

- `key_ref` 由宿主生成（可用独立 UUID），manifest 只能写本包 slot ID；不能提供 OS service、
  任意 key_ref、已有 provider 引用、其他插件 ID 或 account ID。授权检查在查缓存/keyring **之前**执行。
- installation ID 也由宿主生成；批准的同源更新保留该身份，grant 绑定新包摘要。另装同名包、卸载重装、
  更换发布者不能凭相同 plugin_id 自动领回旧槽位；恢复旧归属由宿主显式验证并迁移。
- keyring 的 service/name 命名本身不是 ACL；scope wrapper 按身份与 ownership record 校验。
  正常请求中 Rust 代取当前绑定的 Key 并直接注入，worker 没有 `get/list/export` 或普通 `set(value)`。
  禁止返回其他插件/账户的 `has`，错误不泄漏槽位是否存在。
- `credential.store/use/status/refresh/clear` 五项分别声明；store 只允许宿主私密表单或宿主认证服务写入
  当前实例/账户的新 staging slot，use 不含明文 read，status 只给布尔/过期状态，clear 需要该用户动作。
  宿主自身凭据管理入口仍由宿主权限控制，不借插件 clear 权限操纵全库。
- Token 类别包括反代应用 Token、API key、OAuth access/refresh bundle、session cookie，全部用同一 keyring。
  OAuth bundle 可沿用现有 Console 的序列化形态，但新版本写入独立 staging entry，
  确认 keyring 写入后通过既有绑定 journal/CAS 切换当前引用，不能宣称 OS keyring 与配置拥有跨库原子事务；
  cookie 若需持久也存 keyring，持久 cookie jar/本地文件不另开豁免。
- OAuth 的 PKCE/state/nonce、device-code、callback、token exchange/refresh 都由宿主实现；
  安全输出只有槽位状态/已验证身份投影。HMAC/mTLS 等通过命名 Rust signer 使用当前绑定，不提供任意数据签名 oracle。
- 缓存、刷新锁和迟到结果跟随账户代次/credential revision；使用既有 staging → 验证 → CAS → 补偿。
  轮换成功但 keyring/config 保存失败进入待恢复，不能重复消费已用 refresh token。引用清理扩展
  `secret_refs::referenced_secrets`，检查 live/pending/历史迁移引用后只清理本 owner 的孤儿；不删除兄弟凭据。
- 原有 provider Key 不因安装插件自动继承。若要迁移，宿主明确建立新的本插件归属 staging entry，
  用户确认后换绑定，旧引用按原规则保留；插件不能要求读取“其他的”。
- headless 当前使用 `KeychainSecrets::with_env_fallback`；插件 Token 路径仍要求 **keyring-only**，
  复用该 backend 的无 fallback scoped facade。keyring 锁定/不可用即报告，不退到环境变量或磁盘。
  原有非插件 headless fallback 不给插件暴露；worker 启动清空环境。测试继续用注入的假 CredentialBackend。
- keyring entry 大小/平台限制在输入时校验；超大 OAuth bundle/cookie 拒绝并报告，不能自动拆到明文文件。
- 若需要把凭据代码抽成共享 crate，只做现有实现的边界搬迁与 feature/target 回归，不能为插件重新实现存储。
  Token 输入、key_ref、credential status 及 auth 回包均不在通用 UI/日志投影中，status 需单独授权。

需补的接缝也明确记录：Core `SecretStore` 目前只有 `get`，桌面已有 `CredentialBackend::get/set/delete`
及可分辨的 `StoreError`，不能把 `Option::None` 同时当作不存在/锁定/后端故障。
scoped facade 复用这些实现并返回类型化结果。现有 `opencode/console.rs::resolve` 的 refresh
只更新内存且依赖该服务的 non-rotating refresh token，不能拿它当通用持久 rotation 已完成的证据；
先补通现有 publication/recovery 基底，再把需要 rotation 的服务注册给插件。

### 7.3 SSL/TLS 指纹由 Rust transport 执行

QuickJS 不能凭改变 User-Agent 改出 TLS ClientHello。对外称 **TLS/HTTP 客户端 profile**：

```text
TransportProfile v1
  id + revision + implementation_digest
  target_family + target_version + evidence_status
  tls: versions / cipher suites / extensions / groups / sigalgs / ALPN / GREASE
  http: h1 header ordering 或 h2 SETTINGS / flow-control / pseudo-header order
  identity: user-agent + 有限 header rules
  compatibility: backend / OS / architecture / proxy route
```

- 提供 `transport.profile.use`；一般插件只引用宿主已测试 profile。
  `transport.profile.declare` 可添加数据型 profile，但由 Rust 校验支持的 TLS 算法/扩展/参数范围并编译为后端设置，
  profile 固定摘要。没有上传原生代码或自定义 TLS verifier 的扩展点。
- TLS 1.2+、证书链及主机名校验固定；私有 CA/mTLS 是宿主网络配置与凭据操作，插件无权关闭验证。
- JA3/JA4 是观察摘要，不足以复原全部协议行为，不能接受一条 hash 就宣称完成伪装。
  header、TLS、HTTP/2 必须匹配同一版本 profile；浏览器 profile 不自动等于 Codex/Claude Code 的真实 CLI 指纹。
- 需要 profile 的候选在选路前检查 backend/平台可用性。不可用即候选不可路由；不能发送后发现失败才降级 native 重放。
- 连接池键包含 origin、proxy route、profile revision、CA/mTLS、账户/credential generation；
  禁跨 origin 的 HTTP/2 connection coalescing，cookie、连接认证与 TLS resumption 状态不跨绑定共享。
- ingress 身份与 egress 模拟身份分开存储。User-Agent 和 x-zroutery-client 只是兼容提示，
  限制某客户端访问必须用宿主颁发的客户端凭据，不能用伪造容易的 header 当鉴权。

**候选后端**：调研时 `wreq 0.15.3 + wreq-util 0.1.0` 声明 Rust 1.85，后者依赖范围为
`>=0.15.1, <0.16`；可以作为 MSRV 1.90 spike 的第一候选。最新 `wreq 0.16.3 / wreq-util 0.2.1`
声明 Rust 1.98，不能直接升级进当前合同。

0.15.3 README 另提示 BoringSSL 与 `openssl-sys` 同进程符号冲突；Zroutery native-tls 在 Linux 的路径需格外验证。
因此设计允许独立的**可信** `zroutery-transport-helper` 承载 fingerprint 后端，避免改变既有 native 池及 TLS 链接边界。
它不是 JS worker、不能加载插件，可获得短生命周期的签名发送计划；broker 仍检查 authority、计数与终态。
究竟进程内还是 helper，以三平台链接/证书/代理/流式验证为准；API 不暴露 wreq 类型，不被选型绑死。
旧候选维护状态/漏洞更新也是进入发行依赖前的门槛，低 MSRV 本身不是安全背书。

## 8. Provider、专用客户端反代与结算合同

### 8.1 统一的请求链

```text
客户端认证/限额/协议解码
  → Core canonical IR 与原始约束
  → hard eligibility / 模型与账户报价 / 唯一路由计划
  → 分配 attempt，固定 plugin digest + binding + profile
  → 已绑定 Rust service 编码，复核撤销/钱包/预算/绑定
  → Core 消费发送许可，host 注入凭据并发送
  → 既有 Rust codec/adapter 解析响应，观察原始 status/usage/model
  → Core 流状态机 / egress / 终态持久结算
  → 可选、获准的非秘密 UI 投影（异步）
```

插件提供的是**实例化之前验证的服务绑定**，不是请求钩子。`HostServiceRef` 由宿主查表，
使用当前 `protocol/`、`upstream/`、`server/pipeline/`、`account/` 和 `billing/` 实现，
没有第二套 request/response DTO、流解析器或计量器供 JS 调用。权限快照/epoch 检查在 Rust 内完成。
不允许 JS 在重试时换路由范围、endpoint、账户、tools 或 reasoning；也不因为 UI worker 无响应而重发请求。

为服务注册表命名时直接包装当前 Rust 接口。例如 `host.protocol.openai_chat` 对应已有 Chat 编解码与 Upstream，
`host.protocol.openai_responses` 对应 Responses surface，`host.protocol.anthropic` 对应 Anthropic；
`host.account.newapi`、`host.auth.opencode_console` 分别受当前 feature 与实际实现约束。
注册表暴露哪些服务取决于该构建的真实能力，不能因已有 Gemini 入站 codec 就宣称 Gemini 上游完整可用。
没有必要为了包装一个服务重写其状态机；新私有服务也在原 Rust adapter 扩展点中实现和测试。

合成的有效 provider 必须携带 Rust 内部不可省略的 `PluginBinding` 来源（安装身份、实例、版本和授权 epoch），
不能转换成丢失来源的普通 native ProviderConfig 后走无权限的捷径。普通推理、模型目录、选举探测、
自动审查/视觉辅助、余额/费率、refresh/签到等所有 credential/transport 调用都进入同一带作用域的 resolver。
resolver 校验后继续调用现有 Rust 实现；即使原模块有按 key_ref 的缓存，也必须先检查本绑定授权和 revision。
禁止把配置中的任意 key_ref 当作“已经验证”的证明，避免包装层存在而旁路仍可读凭据。

### 8.2 三种“特定客户端”需求分别接入

1. **把某个客户端专用云后端变成 provider**：如用户指定的 Coding Plan/专用服务。
   插件组合已注册的协议/认证/目录/传输服务及获准 endpoint，应用 Token 由相同 keyring 托管。
   多步骤专用协议如当前没有 Rust adapter，先扩展该基底；不能通过附带 JS decoder 或 DLL 绕过这一条件。
2. **让特定客户端连到 Zroutery**：`ClientDescriptor` 提供发现/配置 schema 和 managed slots。
   宿主用已有 takeover service 生成四档提案、解析受管位置、比较源指纹、展示预览、原子 apply/restore；
   本地 gateway token 和实际配置文件内容不交给 JS，插件 UI 只请求 manifest 已声明的接管动作。
   不把 `~/.ssh/config` 等任意路径作为“客户端配置”授权；新客户端路径/字段 schema 需宿主确认，
   用户自选文件也须匹配 schema，不能通过写字段获得 shell/exec 能力。
3. **客户端只支持专用入口协议**：声明命名路由与已注册 Rust ingress service；宿主先认证/限额，
   再由该 Rust service 进入现有 IR/响应编码流程。若需要额外路径，限定在
   `/v1/extensions/<plugin_id>/<route>`，不能覆盖现有路径、注册 JS handler 或独立监听 socket。
   WebSocket/gRPC 等目标只有在 Rust 基底有对应完整取消/背压/计量合同后才可绑定；按真实服务能力报告支持范围。

客户端可用模型/effort 目录取 Core 的同一 catalogue，公共 class 继续取能力交集；
既有 `ProviderKind` 与 `AgentType` 逐步增加 adapter reference，新供应商名称不再无限增长 Rust enum。
插件缺失时 provider 声明保留并显示不可用，不能把其 native kind 当作自动直连兜底。
**向旧二进制安全降级**：优先把插件实例/绑定放进新增的非秘密顶层 `plugin_instances` 声明区，
原 `providers` 数组只存真正的 native provider。Rust 在 registry 构造时合成有效实例；
旧版会忽略 `plugin_instances`，因而没有这些 provider 可直连。插件 Key 的真实引用不写入旧 `providers`。
相应模型仍可使用已有稳定 ID，旧版找不到所属 provider 时必须确定性不可用，纳入回归。
不能先把插件导出成启用的 `openai_compatible` 再靠一个旧版忽略的 `requires_plugin` 字段保护它。

同时引入可识别的配置版本/只读保护：当前 `store::startup_load` 把解析失败视为损坏并载入默认，
已支持版本检查的新程序必须把“版本过新”与“损坏”分开。已发行旧版无法靠新增 guard 获得新行为，
安全回退的最低保证来自上述“旧 providers 中没有可执行绑定”；降级编辑可能丢新字段，需保留迁移前快照并提示只读/恢复。

### 8.3 流式、失败和不可重放动作

- status/usage/model observation 始终由现有 Rust adapter 产生，插件没有写 usage、failure、
  实际身份或历史的 API。来自第三方的模型/价格声明保留 provenance，不能自动变成已验证有效价。
  宿主解析也不保证远端声明真实，仍遵循已有来源/新鲜度/未知值规则。
- token 数遵守缓存是输入子集、reasoning 是输出子集；缺项用 null/unknown，货币/额度单位分别聚合。
  catalog、rates、quota 是不同 observation，刷新失败保留最近有效值与旧时间，另记最近失败尝试。
- 既有 Rust 流状态机拥有 message/start、delta、tool 生命周期、usage、stop，检查 sequence、
  tool ID/arguments 完整性、Stop 唯一与终态后禁止数据；上游 HTTP 200 不自动等于完成。
- JS 不接收或产出 stream event。Rust adapter 失败时已知部分保留，未知标未决。
  失败/取消的已知费用仍记 attempt，不能把最终成功覆盖前一次成本。
- 执行一条请求及其重试时，QuickJS 调用数和携带正文的 plugin IPC 字节数应为 **0**。
  UI 的异步统计投影须有独立限流与背压，消费不及时不能阻塞或重试真实请求。
- 控制/UI 异常、超时、OOM、worker crash 是 `plugin_*` 故障；隔离该 facet，并保留已验证 Rust 声明的执行能力。
  策略撤销或包摘要损坏才禁用相应服务绑定。真正的上游失败仍由 Core failure authority 分类，JS 无权提交故障提案。
- **发送前**失败可按既有 allowed plan 切换候选。**发送后但无完整响应**属于可能已执行，
  是否 retry/failover 由 Core 的重放安全证据决定；无幂等/未执行证据时不得仅凭“客户端还没收到 token”重发。
  **已交付工具/内容后**数据面错误或明确撤销终止当前流，不能换 provider 偷偷续写；单纯 UI 失败不终止流。
- 本地结算保存失败只重试持久化，不重新调用上游、refresh、签到或已执行工具。
  before-send durable operation intent 与 terminal fact 不构成分布式 exactly-once；强退后的未知远端执行结果仍需核对。

路由仍复用 Core 现有策略与 ML 合同。插件如提供路由配置界面，也只能引导用户使用宿主已发布的配置服务，
当前 v1 没有每请求的 JS 排序/分类/工具执行扩展点。

## 9. UI 扩展与两个主题的完整迁移

### 9.1 声明式 UI

QuickJS `ui_render` 返回 `UiNode`，包括 stack、text、value、table、notice、input、select、button。
UI 更复杂的图表/字段分组在同一有限组件库增补，组件渲染、焦点、键盘、可访问性由宿主负责。
schema/props 不支持 HTML、JS callback、任意 style、CSS selector、外部 URL 图片或富文本脚本。

- slots 为 `overview.extension`、`provider.detail.extension`、`model.detail.extension`、
  `client.detail.extension`、`usage.extension`、`settings.extension` 和设置下的扩展页。
  provider 面板只绑定自己的实例；跨 provider summary 另授权限定投影。
- `projection` 是 broker 冻结的 DTO（作用域、筛选、货币单位、revision、coverage），不能把完整 `AppSnapshot` 给插件。
  请求正文、密钥、原始错误正文、浏览器 session 均不在投影中。
- 动作使用宿主生成的 `UiActionRef`，绑定视图 revision/实体/动作 ID/有效期。
  `ui_action` 只返回 options proposal 或有限 host action；broker 再确认实际用户动作与实体权力，防止程序化伪造点击。
  返回 action ID 必须与点击的 manifest action 完全一致，不能把“刷新”偷换成“删除”。
  action token 单次使用，在触发副作用前消费并关联操作状态；重复/迟到结果只返回现有操作状态，不再执行。
  `save_options` 只写包内非秘密 options，不能重写 host service、endpoint、credential owner 或权限。
  native 密钥/登录/恢复界面显示实际作用和目标，用户确认由宿主持有，插件不能确认自己的提案。
- 密码/授权输入由宿主原生流程承载，值不送回 `UiNode.fields`。插件页显式标注所属插件，
  授权确认、删除、预算拦截和恢复界面在独立宿主安全样式域，插件不能伪装成它们。
- 每插件面板独立 error boundary、节点/表格/消息速率上限；隐藏页面暂停订阅，旧 revision 的异步响应丢弃。
  主 UI 不 `import()` 插件脚本，IPC 也不提供“任意 Tauri invoke”转发命令。

这允许插件增加面板、选项、操作及自己的完整页面，并继承当前主题；不会为每个 provider 建一个独立 UI 框架。
未来确需 HTML iframe 必须单独立项：opaque origin、CSP、导航外传、postMessage/source 校验和无 Tauri ACL 都需实测。
只设置 `connect-src 'none'` 仍挡不住所有 self-navigation 外传，因此 v1 的敏感投影不发给自带 HTML。

### 9.2 ThemeV1 是 token + 组件配方

主题不是可执行脚本，也不是对主窗口注入任意 CSS。包含：

- `modes.light/dark.colors`：语义颜色角色；颜色以 hex/ref/system-accent/mix 的有限表达式描述。
  宿主解析为具体值，限制引用深度、禁止循环/URL/CSS 字符串与无限计算。
- `recipes`：宿主发布的 part/state 集合，覆盖按钮各 variant、输入/Select、checkbox/switch、
  navigation/segment、section/嵌套 section、drawer/dialog/popover/menu、toast、KPI/chart、scrollbar。
- 配方属性是受限的颜色、描边、圆角、阴影、state-layer、选中指示、尺寸和有限 `filled_section/flatten_nested` 布局策略。
  Fluent acrylic 用宿主 material 配方的有界 opacity/blur 表达，不通过插件提供 CSS 函数字符串。
  几何按 component part 的边界校验，不能设置全屏遮罩、隐藏安全控件、任意 z-index、pointer-events 或 CSS content。
- 字体引用宿主的 `industrial / roboto / segoe_ui` 资源角色，系统缺字体时由宿主 fallback；
  本批不允许插件上传可执行字体解析面或远程字体。字体许可证继续随应用分发。
- 配方使用 part/variant + 状态谓词，覆盖 checked+hover、selected+focus、readonly、嵌套区域等组合，
  不能用互斥的单个 state 枚举丢掉当前样式。多条匹配采用宿主固定优先级，冲突/未知 part 在安装时拒绝。
  hover/pressed/focus/disabled/selected 为正式状态，不只验 resting；焦点内外环使用类型化 rings。
  强制色/减少动态等宿主偏好优先。
  当前用户批准的 raw system accent 语义保留，其 contrast 例外如实记录；不能顺迁移悄悄重新配色。
  安全确认/密钥流程不采用外部主题的颜色，具有独立高对比兜底。

拟议包：

```text
plugins/theme-md3/zroutery.plugin.json
plugins/theme-md3/theme.json
plugins/theme-fluent/zroutery.plugin.json
plugins/theme-fluent/theme.json
```

两包均 `runtime: null`，只声明 `theme.contribute`。官方主题与第三方主题经过同一 validator/renderer，
官方身份不提供任意 CSS 后门。宿主编译器掌握 component part → CSS/React 的映射，
不能在宿主继续写 `if theme_id === md3` 然后仅把色板放进插件，制造形式上的插件化。

### 9.3 迁移顺序与恢复

1. 冻结当前 `styles.css` 的 layer-2 + **全部 layer-3** 规则、实际首帧与三皮肤×亮暗的 computed styles/截图。
   记录当前已知差异，历史 UI 验收仍开放的项目不能被此次“迁移”顺带宣称通过。
2. 建立语义 part/state/recipe 表，覆盖 MD3 tonal 按钮、层级表面/暗色 tint、36px 控件/section 卡片，
   Fluent acrylic、双焦点环、NavigationView 选中指示、40×20 固定 knob 开关、32px 控件及卡片层。
3. 两套样式逐项迁入 data packs，通用宿主 renderer 用配方替代现有 skin selectors；所有路由/表单共用这一个实现。
4. `Skin = "industrial" | "md3" | "fluent"` 改为稳定 ThemeRef 注册表；旧 `zroutery-skin=md3/fluent`
   幂等映射到 `org.zroutery.theme.md3/fluent` 的已安装版本，`zroutery-theme` 的 system/light/dark 独立保留。
   Appearance 保存的仍是阅读偏好，不混进 provider 配置。
5. MD3/Fluent 随安装器离线带上，使用者无需再次下载。构建期对 bundled data 编译安全样式；
   已安装外部主题由宿主验证后生成私有只读缓存，启动以 content hash/renderer version 检查后注入首帧。
   localStorage 只保存 theme reference，不能把其中任意字符串当 CSS 注入。
6. 新样式验证/构建成功后原子切换。包禁用、损坏、schema 过新或载入超时立即用 Industrial；
   把“首选主题”和“当前回退主题”分开，恢复后可回到用户选择，失败不能写坏旧偏好。
7. 导出/导入偏好只带引用和非秘密选项；主题卸载不删除用户偏好/业务数据。
   提供 native 菜单/启动参数进入安全外观与禁用插件，恢复入口不依赖失效的扩展 UI。

按已接受语义检查英/简中/繁中、native accent 与三皮肤×亮暗各页面、820×560、系统缩放与键盘。
原布局桩只验证 Industrial；MD3/Fluent 需要新增按其高度/配方的真实 layout/computed-style 断言，不能引用旧桩作六态验收。

## 10. 安装、更新、禁用与恢复

### 安装与版本发布

```text
下载/选择文件 → staging 验包 → 权限/影响预览 → 用户启用
  → 建立沙箱与纯注册校验 → atomic active generation → 可被实例引用
```

- 工件检查 zip-slip、绝对路径、`..`、符号/硬链接、Windows reparse/ADS/device 名、大小写冲突、
  文件数/总解压大小/压缩比、重复文件与签名摘要；读取包使用 handle-based/no-follow 或不可变字节快照，防 TOCTOU。
- UI 图片/icon 只取批准包内经宿主解码的位图/内建图标；没有脚本 SVG、远程资源或能借资源加载网络的路径。
- ID 冲突显式拒绝或让用户迁移 owner；内建 `org.zroutery.*` 命名空间受包来源绑定保护，
  本地包不能借相同 ID 覆盖官方主题，显示名不作为身份。
- `requires` 只表达版本化宿主服务/可用 feature，不自动下载安装依赖插件。启用顺序确定且检测 cycle；
  v1 不开放任意插件间 RPC，后续若开放需双方能力与数据流预算。
- 更新由用户触发，精确包摘要锁定，不后台执行新 code；变更 origin、宿主服务、凭据用途、UI 数据范围或 TLS profile
  即重新展示权限差异。权限不随“同发布者”无限继承；新摘要由宿主按用户批准的权限复制出新 grant。
- options migration 使用独立无网络/无凭据的数据 Runtime，对副本运行并验证；成功后 CAS 提交。
  migration schema/downgrade 不兼容则拒绝回滚配置或恢复对应旧副本，不能用插件脚本回滚账本。

### 禁用不是重新安装

- 普通更新：新 generation 只接新请求，旧 generation 在有界 drain 中完成已有操作；每条日志/attempt 保存实际摘要。
- 安全撤销/禁用：先原子提升 grant epoch、阻止新 capability/出站，再取消现有调用与网络、关闭流，最后杀 worker。
  不能 drain 完再撤销，不能只从 UI 隐藏。已送到上游的操作无法撤回，按实际已观察事实结算/标未知。
  发送许可与撤销共用 Rust 同步边界，单次许可在注入凭据/交给 transport 时消费；
  先获得许可的操作视作已开始并立即接受取消，撤销后的新操作一律拒绝，不能“检查 epoch 后再无锁排队发送”。
  用户收到持久禁用成功之前，授权记录必须完成原子持久化；保存失败立即在内存拦截并返回“禁用未持久”，
  不把本次会话已停误报成重启后也保持。授权库打不开/版本过新时启动拒绝激活扩展，不用默认全开状态。
- worker 被中止后，host 仍持有 in-flight send/observation/settlement 责任；销毁 JS 不能连带遗失已发生的费用。
- 普通关闭 provider contribution 后对应实例保留、变为不可路由；主题关闭回退 Industrial；UI contribution 移除。
  要删除凭据或恢复客户端文件是独立宿主动作，客户端恢复仍遵守 stale-file/CAS，不强行覆盖用户后改内容。
- repeated crash/timeout 只熔断对应控制/UI facet；权限违规则按 grant 作用域撤销相关动作或绑定。
  整包被用户禁用时才同时撤销全部贡献。退避不自动重放旧操作，修复后由用户重启。
- crash recovery 以 host durable operation intent/结算事实辨别未发送、已发送未决、已结算，
  不能把 worker “没回 RPC”视为“上游没执行”。

## 11. 落地切片与依赖

| 切片 | 主要产物 | 退出条件 |
| --- | --- | --- |
| P0 合同与 spike | API/schema、capability/IPC threat model，rquickjs Windows/MSRV、OS 沙箱、TLS backend spike | 冻结真实支持矩阵；代码执行前沙箱负例；避免依赖/链接与 API 误承诺 |
| P1 宿主与 worker | 三 crate、不可变包、broker、进程/Runtime 隔离、配额/撤销/状态 | 恶意 fixture、OOM/死循环/IPC/跨账户/崩溃恢复定向通过；native 最小构建通过 |
| P2 主题插件 | ThemeV1 编译器、注册表、MD3/Fluent data packs、旧偏好迁移 | 全 layer-3 保真、三皮肤六态/原生 accent、禁用/损坏/首帧恢复 |
| P3 Provider 与客户端 | 现有 Rust service registry、scoped keyring、协议/auth/takeover 绑定 | 合成服务商端到端、数据面零 JS、应用 Token 隔离、唯一尝试/结算、精确 effort/工具、接管 CAS；再接用户选定的真实目标 |
| P4 TLS profile 与 UI | fingerprint TransportPort、profile registry、UI slots/actions/projections | 指纹实测/证书负例/代理边界、无未授权出站；声明式 UI 及权限差异交互 |
| P5 发行验收 | 桌面/headless 的管理、SDK 示例、包升级/rollback、完整仓库 gates | 最终构建与分平台证据、性能/安装/禁用/恢复，不以本地通过替代发布 |

P2 的数据主题只依赖 P0 的主题合同和注册/验证底座，可以先于可执行第三方插件发行。
P3/P4 按上述依赖推进，真正需要 TLS 的 provider 在 profile 未验收前保持不可路由。
不把其他已接受升级里程碑整体标完成，也不把本插件系统追加为既有 RC 首批退出的隐含前置。

建议新增/修改位置（拟议路径）：

```text
crates/zroutery-plugin-api/src/{manifest,capability,provider,ipc,theme,ui}.rs
crates/zroutery-plugin-host/src/{install,supervisor,broker,grants,lifecycle}.rs
crates/zroutery-plugin-worker/src/{main,runtime,loader,sandbox,dispatch}.rs
crates/zroutery-core/src/plugin/{mod,registry,bindings,grants}.rs
crates/zroutery-core/src/upstream/transport.rs
plugins/theme-md3/ + plugins/theme-fluent/
ui/src/plugins/{registry,renderer,actions,manager}.tsx
ui/src/themes/{registry,compiler,preferences}.ts
src-tauri/src/plugins/{commands,scoped_secrets,client_bindings}.rs
scripts/plugin_contract_test.py + scripts/plugin_sandbox_test.py
```

可选 transport helper 在 P0 spike 后确定。新增目录不应机械复制旧架构；接口边界和测试可继续沿现有模块拆分。
修改 workspace manifest、Cargo.lock、API types/fixtures/evidence registry 前重新核对共享文件。
IPC 先 `ui/src/api.ts` 冻结 DTO → Rust → `ui/mock-bridge.ts` 同步；commands 必须同时注册到
`src-tauri/src/lib.rs`、`src-tauri/build.rs` 并只给相应 main window 权限，不能给 tray 通用插件操作权。

拟议管理动作：inspect package、install staged package、preview grants、activate/disable、list status、
preview/update/rollback、render projection、invoke approved UI action、remove package。
不提供 `eval_js`、任意 filesystem path、任意 host call 转发 IPC。headless 复用同一 broker/授权记录，
非交互模式未授权的新能力必须失败，不隐式接受。

## 12. 验收：让安全主张可以被反例推翻

### 定向测试

- **包/加载器**：zip-slip、ADS/reparse、case collision、symlink race、ESM 越界/URL import、native module、
  篡改摘要、第三方 bytecode、模块初始化超限；不触及真实用户目录。
- **JS/进程**：死循环、无限 Promise、深递归、巨大 ArrayBuffer/字符串、超长 getter/toJSON、RegExp 耗尽、
  worker native crash；host watchdog 真能终止，主网关仍服务；跨 Runtime/账户状态不可读。
- **OS 负例**：worker 直接 syscall 访问网络、文件、其他进程/凭据/子进程均失败。
  用受控测试 helper 证明 OS 限制，不用“JS 没暴露 fs”冒充沙箱验收。
- **broker**：缺少任一 manifest 权限即拒绝；声明未授权、伪造 action handle、跨账户/跨 facet、旧 grant epoch、
  注销/rekey/删除重建、迟到 RPC、批量 flood、取消竞态、UI action 重放/伪造与 registry 冲突。
- **网络**：DNS rebinding、IPv4-mapped IPv6、302/307 跨 origin、Host/header 注入、压缩炸弹、
  代理 DNS、带 auth 的 private endpoint、同源 catalog→推理路径滥用、自己的网关递归。
- **认证**：两插件×两账户不同 token/身份/余额；跨 owner 读/use/status/store/delete 均拒绝，缓存命中也先鉴权；
  single-flight refresh、rotation 保存失败、state/PKCE 不匹配、profile/cookie pool 隔离；
  app token/oauth/cookie 均调用现有 keyring backend，锁定失败不落文件/环境变量；生产日志/dump 不含 fixture secret 或正文。
- **数据面边界**：给请求/响应/认证数据放置唯一哨兵，检查全部 JS 入参与 IPC 不出现；
  unary、stream、重试、工具及 cancel 都没有 QuickJS 调用，杀死 UI worker 后请求仍正常完成且账目一致。
- **协议/计量**：native/私有、stream/unary、取消/早断、duplicate Stop、工具已交付、已发送未回包、
  所有失败尝试的已知费用、unknown 与 0、缓存/reasoning 子集、不同币种、失效费率、保存失败不得重发。
- **客户端**：命名 Rust ingress service 先鉴权、四档/effort 交集、配置预览/过期/CAS/恢复、字段被手改、目录升级，
  native 菜单生效与观测实际请求分别验收。
- **TLS**：本地受控 TLS server 抓 ClientHello/ALPN/HTTP2 SETTINGS/header 顺序，
  正负证书/错主机名/过期、直连/代理、连接复用/取消、target profile 与原客户端实测证据；
  browser hash 相同不宣称某 CLI 或反机器人系统必然通过。
- **UI/主题**：恶意文本/节点、不能 invoke/联网/覆盖安全 UI；全部组件状态、三皮肤×亮暗、
  820×560/缩放/长翻译、隐藏页订阅、旧偏好/坏包/关插件/强退更新与首帧。

### 仓库与性能

遵守 AGENTS.md 的工具链、feature 和命令面 gates：先运行 toolchain gate，使用其解析的当前 CI Rust；
再 fmt/clippy/workspace all-feature tests、Rust 1.90 locked all-target check、Core/desktop no-default
及 feature matrix、UI build/test/budget/layout/interaction、command surface、docs 和 `git diff --check`。
单独核查 `cargo tree -e features` 中 worker 没有 `dyn-load / full / rust-alloc` 意外 feature unification。

性能在同一合成数据集比较 native、同服务的 plugin binding、TLS profile，以及 UI worker 高负载/失效时的路径：
50/200 QPS、10k-token 流、1k burst；记录 cold/warm、P50/P95/P99、RSS/CPU、TTFT、取消时延、
背压和 pending settlement。数据面 plugin IPC/JS 调用必须为 0；控制/UI 并发不抢占 Core executor。
绑定校验带来的开销要测量，但不为优化绕过撤销 epoch、账户作用域和唯一结算。

Windows 本机、macOS 原生 sandbox/签名与 Linux runtime 分开记录。数据型绑定/主题可独立发布，
不可执行 JS 的平台明确标注扩展 UI 不可用。**设计完成、实现、定向通过、全量通过、提交、发行是不同状态。**

## 13. 审阅时应确认的产品决策

本草案推荐：独立 OS 沙箱、原生安全 API、不兼容宿主特权的 OpenCode/Bun 插件原包；提供迁移指南与作者侧兼容辅助。
用户已指定的数据面复用、keyring-only 及权限显式声明是固定约束。主题采用完整配方的数据包；
UI 使用宿主组件；TLS 使用受约束 profile；先通过合成 provider 验收。
首个真实专用后端、客户端入口/版本、认证方式和可用账户需用户指定，才能补该目标的协议夹具和现场验收。
不凭“Codex/Claude”名字推定其后台协议、支持的 effort 或可用 TLS profile。

该草案经审阅后冻结 schema 与实施切片；后续扩展仍受上述固定约束，不能增加 request-read、JS decoder、
通用 fetch 或跨插件 keyring 权限作为“兼容性例外”。

## 14. 参考来源（2026-11-13 读取）

- [Magpie 官方插件文档](https://usemagpie.ai/docs/plugins)：provider/auth、Bun、moejs、agent 与 UI 页面安全边界。
- [Magpie 内建插件运行时（固定 SHA）](https://github.com/yetone/magpie/blob/92bce5c724bcc857474a7ba5c135f0d36efecd31/internal/pluginrt/pluginrt.go)。
- [Magpie provider 桥（固定 SHA）](https://github.com/yetone/magpie/blob/92bce5c724bcc857474a7ba5c135f0d36efecd31/internal/provider/plugins.go)。
- [rquickjs 0.14.0 API](https://docs.rs/rquickjs/0.14.0/rquickjs/)、
  [Runtime 限额与 interrupt](https://docs.rs/rquickjs/0.14.0/rquickjs/struct.Runtime.html)、
  [features/MSRV](https://docs.rs/crate/rquickjs/0.14.0/source/Cargo.toml)、
  [QuickJS-NG 与平台说明](https://docs.rs/crate/rquickjs/0.14.0/source/README.md)。
- [QuickJS 手册](https://bellard.org/quickjs/quickjs.html)：Context/Runtime、第三方 bytecode、内存与中断机制。
- [wreq 0.15.3 README](https://docs.rs/crate/wreq/0.15.3/source/README.md)：fingerprint、兼容版本及 TLS 符号冲突。
- [wreq 当前版本元数据](https://crates.io/api/v1/crates/wreq)、
  [wreq-util 当前版本元数据](https://crates.io/api/v1/crates/wreq-util)、
  [wreq-util 0.1.0 依赖范围](https://crates.io/api/v1/crates/wreq-util/0.1.0/dependencies)。

### 本次交付与验证

本次交付为设计文档、TypeScript API 草案及编译期边界断言；P0–P5 是后续实施切片。
运行时、主题迁移、Cargo 依赖、平台沙箱/TLS 和实际请求验收仍待实施，不能以类型检查通过代替安全验收。

2026-11-13 本地检查：

- **通过**：TypeScript 7.0.2 严格检查 API 和 8 项编译期断言；权限 scope 必填、显式权限列表、keyring-only、
  仅四类控制/UI hooks 与 IPC、无 request/response 或明文凭据 API。
- **通过**：提取文中的 manifest，验证 JSON 无重复 key、三语言显示名、本例服务/凭据所需权限全部显式覆盖，
  再用 `satisfies PluginManifestV1` 做 TypeScript 正例检查；本次三文件的本地链接和 whitespace 检查。
- **通过**：`git diff --check`。
- **自测通过，仓库校验失败**：`uv run --python 3.12 --no-project python -B scripts/orch_docs_test.py --self-test`。
  现有状态记录仍引用已不存在的 `ui/src/pages/MlRouting.tsx`、`ui/src/components.tsx`，
  对应已有项目待办；本次新文档链接没有报错。
- 初次独立 tsc 调用因 TS7 要求 `--ignoreConfig` 而退出；加上该参数后上述检查通过。
  本次没有运行 Rust/产品 UI 全量 gates，因为产物是设计与声明类型。

可重复的类型检查命令：

```text
pnpm --dir ui exec tsc --ignoreConfig --noEmit --strict --skipLibCheck false --target ES2022 --module ESNext --moduleResolution bundler ../docs/development/plugin-api-v1.d.ts ../docs/development/plugin-api-contract.test-d.ts
```

## 15. 2026-11-14 附注：主题数据与宿主行为层的 v1.1 增量

- **ThemeV1 增加可选配方字段 `stroke_gradient`**（§9.2 的 WinUI
  ControlElevationBorderBrush 落地）：与 `stroke` 互斥、必须有 `fill`，
  编译为透明边框 + 双层 background 的类型化渐变环；additive、向后兼容，
  定义见 `plugin-api-v1.d.ts`，对账边界见 `skin-spec-md3-fluent.md` v3.2。
- **宿主行为层标准化**：interactive 组件（confirm/drawer/menu/toast/tabs/
  segment）的键盘、焦点、portal、层叠全部交给 Radix 无头原语，主题包继续
  经 parts/recipes 拥有全部视觉；宿主无 per-theme 分支的约束不变。主题包
  的每个值此后由 `scripts/plugin_theme_source_check.py` 对账到钉住的官方
  实现源（`scripts/theme_sources/manifest.json`）。
- **按皮肤布局断言**：`ui_layout_test.py --skin industrial|md3|fluent` 落地
  §9.3 的要求——MD3 36px、Fluent 32px 的控件高在各自皮肤下实测。
- 本批未新增 IPC：主题流沿用既有 `get_plugin_themes`；无 Rust 改动。

提交整合补充：Rust `ComponentRecipe` 原先遗漏 `stroke_gradient`，导致同一
Fluent 包无法通过宿主校验。现已同步受限字段、fill/互斥/边界校验与正负
回归；旧字段保持兼容，未知字段继续拒绝。
