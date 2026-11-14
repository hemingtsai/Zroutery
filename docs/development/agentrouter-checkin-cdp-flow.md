# AgentRouter 每日签到：CDP 控制任务流程

记录时间：2026-11-14（Asia/Shanghai）
记录环境：本机 Windows + Chrome 157.0.8103.38 + CDP 端口 9222
记录方式：真实 Chrome 实例上实际登录一次 + 前端 bundle 与接口实测取证

## 1. 结论

1. AgentRouter（New API 发行版）**没有独立的「签到」按钮或页面**。签到在**登录成功的那一刻由服务端结算**：登录响应返回的用户对象里带 `checked_in` 布尔字段，前端据此弹出「签到成功，新增额度已到账」。
2. 因此「每日签到」的完整动作是：**先注销，再用 GitHub OAuth 重新登录**。同一天内重复登录 `checked_in` 为 `false`，不会重复发放。
3. 站点只开放 GitHub / LinuxDO 两种 OAuth 身份，GitHub 账号走 `https://github.com/login/oauth/authorize?client_id=Ov23lidtiR4LeVZvVRNL&state=<state>&scope=user:email`，授权完成后在**新标签页**回到 `https://agentrouter.org/oauth/github?code=<code>&state=<state>`。
4. 自动化能做的是「打开登录页 → 点 GitHub 按钮 → 等回调 → 断言 checked_in」，**GitHub 账号密码那一步必须由人完成**，不得脚本化输入凭据。

## 2. 环境前置条件（Chrome 157 的硬限制）

- Chrome 136 之后，用**默认 user-data-dir** 启动时 `--remote-debugging-port` 会被忽略。实测：直接对默认 profile 加端口，`/json/version` 无响应。
  - 可行做法：把 profile 复制到非默认目录再启动。
  - 本文实测命令：
    ```
    chrome.exe --remote-debugging-port=9222 ^
      --user-data-dir="%TEMP%\agentrouter-cdp-profile" ^
      --profile-directory=Default --no-first-run --no-default-browser-check
    ```
- 复制 profile 时，运行中的 Chrome 对 `Default\Network\Cookies` 持有独占锁，读不出来（`ERROR 32 / Permission denied`）。
  - 后果：新实例**没有登录态**，必须重新登录一次。
  - 想免登录只能先退出原 Chrome 再复制 cookies，代价是中断用户原来的浏览会话。
- 校验端口可用：`GET http://127.0.0.1:9222/json/version` 应返回 `webSocketDebuggerUrl`。

## 3. 页面与接口事实

| 对象 | 值 | 说明 |
|---|---|---|
| 登录页 | `https://agentrouter.org/login` | 三个入口：GitHub / LinuxDO / 邮箱或用户名 |
| 登录按钮文案 | `使用 GitHub 继续` | Semi Design 按钮，`button` 内文本 |
| OAuth state | `GET /api/oauth/state?aff=<aff>&mode=login` | 返回 `data` 即 state 字符串 |
| 授权跳转 | `window.open("https://github.com/login/oauth/authorize?client_id=...&state=<state>&scope=user:email")` | 新标签页，`localStorage.oauth_mode=login` |
| 回调路由 | `https://agentrouter.org/oauth/github?code=<code>&state=<state>` | SPA 路由 `/oauth/github` |
| 用户自省 | `GET /api/user/self` | 必须带请求头 `New-Api-User: <user_id>`，否则 403「无权进行此操作，未提供 New-Api-User」 |
| 注销 | `GET /api/user/logout` | 前端随后清 `localStorage.user` 并跳 `/login` |
| 控制台 | `https://agentrouter.org/console` | 侧边栏：数据看板 / API令牌 / 使用日志 / 模型状态 / 钱包 / 个人设置 |
| 登录态载体 | `localStorage.user`（JSON） + HttpOnly session cookie | `localStorage.user` 含 `id / username / quota / used_quota / checked_in / last_login_time` |
| 成功提示 | `登录成功！` / `签到成功，新增额度已到账` | 二选一，取决于 `checked_in` |
| 额度单位 | 1 USD = 500000 quota | 页面与接口比值一致；个人余额已移除 |
| 每日签到额度 | `$25`（= 12,500,000 quota） | 来自站点公告「支持登录签到；签到送 $25 Credit」 |

## 4. CDP 执行流程

坐标/颜色类操作全部不用，纯 DOM + 网络断言。

### 步骤 0：连接

```text
GET  http://127.0.0.1:9222/json/version        -> webSocketDebuggerUrl (browser 级)
WS   browser 级连接
     Target.setDiscoverTargets {discover: true}
     Target.getTargets
```

### 步骤 1：打开登录页

```text
Target.createTarget {url: "https://agentrouter.org/login"}   # 已有同 URL target 时直接复用
Target.attachToTarget {targetId, flatten: true}               # 或直接连该 target 的 webSocketDebuggerUrl
Page.enable / Network.enable / Runtime.enable
断言：Runtime.evaluate("document.readyState") === "complete"
```

### 步骤 2：触发 GitHub 授权

```text
Runtime.evaluate:
  [...document.querySelectorAll('button')]
    .find(b => b.innerText.includes('使用 GitHub 继续')).click()
```

前端实际行为（顺序固定）：

```text
GET /api/oauth/state?mode=login          -> {success:true, data:"<state>"}
window.open("https://github.com/login/oauth/authorize?client_id=Ov23lidtiR4LeVZvVRNL&state=<state>&scope=user:email")
```

断言：

```text
Target.getTargets 中出现 url 匹配 github.com/login/oauth/authorize 的 page target
```

### 步骤 3：GitHub 授权（人工步骤）

- GitHub 已登录会话下：页面直接显示 `Authorize` 按钮，点一次即可。
- 未登录：必须由人输入账号密码 / 过 2FA，**自动化到此为止**。
- 断言：出现 `agentrouter.org/oauth/github?code=...&state=...` 的 target。

### 步骤 4：回调结算（签到真正发生的地方）

```text
回调页 GET /api/oauth/github?code=<code>&state=<state>
  -> 校验 state、换 access_token、下发 session cookie
  -> 同一响应返回用户对象，其中 checked_in 为本次是否结算
前端：localStorage.setItem('user', JSON.stringify(user))
       checked_in ? toast("签到成功，新增额度已到账") : toast("登录成功！")
断言：
  Runtime.evaluate("JSON.parse(localStorage.getItem('user')).checked_in")
```

### 步骤 5：校验额度

```text
Runtime.evaluate:
  fetch('/api/user/self', {credentials:'same-origin',
    headers:{'New-Api-User': String(JSON.parse(localStorage.getItem('user')).id)}})
    .then(r => r.json())
断言：success === true，且 quota 相比签到前 +12500000（跨天场景）
```

### 步骤 6：收尾

```text
Target.closeTarget {targetId: <github target if still open>}
WS close
```

### 跨天（真正的「每日」循环）

```text
GET /api/user/logout                    # 或 UI：控制台头像按钮 -> 「注销」
                                        # 前端注销实现：await GET /api/user/logout
                                        #   -> toast「注销成功!」-> 清 localStorage.user -> 跳 /login
                                        # 所以调用接口后不需要再手动清 localStorage
Page.navigate "https://agentrouter.org/login"
回到步骤 2
```

关键点：**不注销直接重登，`checked_in` 为 `false`，当天不会重复发放**。注销 + 重登才是站点认可的签到动作。

## 5. 幂等与异常处理

| 现象 | 判定 | 处理 |
|---|---|---|
| `checked_in === false` 且当天已签过 | 正常，不重复发放 | 视为已完成，不再重试 |
| `checked_in === true` | 本次登录完成签到 | 记录 `last_login_time` 与 quota 增量 |
| `403 未提供 New-Api-User` | 少请求头 | 从 `localStorage.user.id` 补 `New-Api-User` |
| 跳 `/login?expired=1` | 会话过期 | 走完整登录流程 |
| 点 GitHub 按钮无反应 | `/api/oauth/state` 失败或 Turnstile 未就绪 | 重取 state；密码登录才需要 Turnstile，GitHub OAuth 不需要 |
| state 不匹配 / 回调报授权失败 | state 过期或被复用 | 重新 `GET /api/oauth/state`，丢弃旧回调页 |
| 回调落在新标签页 | OAuth 用 `window.open` | 必须 `Target.getTargets` 按 URL 匹配新 target，不能假设当前页 |
| 同 URL 多 target | 残留登录页/控制台 | 以最后一个匹配 `oauth/github` 的 target 为准，其余 `Target.closeTarget` |

## 6. 安全边界

- 不读、不导出、不落盘 `HttpOnly` session cookie、`access_token`、`localStorage.user.password`；日志里对 `access_token`/`password` 掩码。
- 不脚本化输入 GitHub 密码、2FA、Turnstile token。
- `state` 属于一次性票据，只在当次内存中使用，不写进文档或证据文件。
- 复制 profile 得到的目录是临时副本，用完应清理（本机路径 `%TEMP%\agentrouter-cdp-profile`）。

## 7. 本次实测记录

| 项 | 结果 |
|---|---|
| CDP 端口 | `127.0.0.1:9222` 可用，`Chrome/157.0.8103.38` |
| profile 复制 | 4.36 GB / 1622 文件成功；8 个 `Sessions_Encrypted\Tabs_*` 因占用失败（无影响）；`Default\Network\Cookies` 独占锁定，复制失败 |
| 登录方式 | GitHub OAuth，人工在浏览器完成授权；回调打开新标签页 |
| 登录后状态 | `localStorage.user.checked_in = true`；个人登录时间已移除 |
| 余额 | 接口与页面单位换算一致（500000 quota/USD）；个人余额已移除 |
| 签到入口 | 控制台各页面均无「签到」按钮；站点公告确认「支持登录签到；签到送 $25 Credit」 |
| 注销入口 | `GET /api/user/logout`；前端提示「注销成功!」并跳 `/login` |

## 8. 未验证项

1. 「签到成功，新增额度已到账」toast 未在自动化链路中被直接捕获——本次登录由人工在浏览器完成，自动化只读取了 `checked_in` 结果。
2. 注销 + 重登的完整跨天循环未在本次执行（会导致会话中断），流程依据前端 bundle 代码与接口契约推导，未做端到端跑通。
3. LinuxDO 登录未验证。
4. 密码登录通道（`POST /api/user/login?turnstile=<token>`）未验证；GitHub 账号的 `original_password` 为空，实际不可用。

## 9. 附：最小可执行脚本骨架

```python
import json, time, urllib.request, websocket

HTTP = "http://127.0.0.1:9222"

def targets():
    return json.loads(urllib.request.urlopen(HTTP + "/json/list", timeout=10).read())

class Page:
    def __init__(self, ws):
        self.ws = websocket.create_connection(ws, timeout=30, max_size=None, suppress_origin=True)
        self.i = 0
    def send(self, method, **params):
        self.i += 1
        self.ws.send(json.dumps({"id": self.i, "method": method, "params": params}))
        while True:
            msg = json.loads(self.ws.recv())
            if msg.get("id") == self.i:
                if "error" in msg:
                    raise RuntimeError(msg["error"])
                return msg.get("result", {})
    def ev(self, expr):
        r = self.send("Runtime.evaluate", expression=expr, returnByValue=True, awaitPromise=True)
        return r.get("result", {}).get("value")

def page_for(url_part):
    for t in targets():
        if t["type"] == "page" and url_part in t["url"]:
            return Page(t["webSocketDebuggerUrl"])
    return None

page = page_for("agentrouter.org")
page.send("Page.enable"); page.send("Runtime.enable")
page.send("Page.navigate", url="https://agentrouter.org/login")
time.sleep(2)

# 1) 触发 GitHub 授权（会开新标签页）
print(page.ev("""(() => {
  const b = [...document.querySelectorAll('button')]
      .find(x => x.innerText.includes('使用 GitHub 继续'));
  if (!b) return 'button not found';
  b.click(); return 'clicked';
})()"""))

# 2) 等人工授权完成后，回调页会出现 oauth/github target
deadline = time.time() + 300
while time.time() < deadline:
    cb = page_for("oauth/github")
    if cb:
        print("callback url:", cb.ev("location.href").split("?")[0])
        print("checked_in:", cb.ev("JSON.parse(localStorage.getItem('user')).checked_in"))
        break
    time.sleep(5)

# 3) 额度核对
page = page_for("agentrouter.org")
print(page.ev("""(async () => {
  const uid = JSON.parse(localStorage.getItem('user')).id;
  const r = await fetch('/api/user/self',
    {credentials:'same-origin', headers:{'New-Api-User': String(uid)}});
  const j = await r.json();
  return j.success ? {quota: j.data.quota, used: j.data.used_quota} : j.message;
})()"""))
```

## 10. 与仓库的关系

本流程描述的是上游站点 `agentrouter.org` 的账号内动作，不涉及 Zroutery 的协议实现。若后续要做成上游账号运维能力，接入点应是账号生命周期（登录/额度）而不是请求转发，且必须遵守「不持久化上游凭据」的约束：session cookie 与 access_token 属于运行时凭据，不得写入配置、历史、日志或证据文件。
