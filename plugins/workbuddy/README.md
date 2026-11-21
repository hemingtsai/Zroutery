# WorkBuddy（CodeBuddy）中继插件

把 WorkBuddy（腾讯 CodeBuddy，`www.workbuddy.cn`）注册为 Zroutery 的对话提供方。
声明走插件 API（`host.protocol.openai_chat` + `host.auth.bearer`），核心不含任何
workbuddy 专属代码；请求身份通过 manifest 的 `extra_headers`（静态非密身份头，
名称来自宿主白名单）表达。

## 打包

```powershell
python plugins\pack.py workbuddy
# 产物：plugins\workbuddy\dist\workbuddy-0.1.0.zrp
```

## 安装与接线

1. 设置 → 插件 → 安装插件包 → 选 `workbuddy-0.1.0.zrp`，批准列出的权限。
2. 「提供方」页面 → 添加 WorkBuddy 连接（插件提供方选择器）。
3. 在提供方抽屉里保存**访问令牌**（存入系统 keyring；令牌只会发送给
   `www.workbuddy.cn`）。访问令牌来自 WB2API 面板的账号凭据或 WorkBuddy CLI 登录。
4. 在提供方里添加账号可用的模型（手动或「拉取模型」），再把它接入分流层。

## 请求形状（与官方 CLI 对齐）

- `POST https://www.workbuddy.cn/v2/chat/completions`，OpenAI chat wire
- `Authorization: Bearer <访问令牌>`
- 身份头：`User-Agent: WorkBuddy/5.11.3 WorkBuddy/5.11.3 CLI/2.176.0`、
  `X-IDE-Version: 5.11.3`、`X-Client-Version/Platform/Product`（CN 5.11.3 新增）、
  `X-Requested-With: XMLHttpRequest`、`Accept: application/json`、`X-No-User-Id: true`
- 路径容错：manifest 声明 `/v2` 基座时宿主不再追加 `/v1`（`versioned_suffix` 通用规则）

## 已知边界（第一版）

- **TLS 指纹**：核心经 reqwest（Windows 走 schannel）出站，ClientHello 不是官方
  Node/OpenSSL 形状。WB2API 面板的抓包显示网关按 JA3/JA4 过滤——如果对话被
  403/WAF 拦截，就需要把 wreq（已验证可精确复刻官方 JA3，见
  `SPIKE-FINDINGS.md`）引入核心，涉及 MSRV 1.98 的依赖决策。
- **令牌刷新 / 签到 / 账号池**：本插件只携带访问令牌；刷新与调度仍在 WB2API
  面板侧，或等待核心账号适配器切片。
- **gzip body**：大请求体的 `Content-Encoding: gzip` 未实现（小对话明文即可）。
