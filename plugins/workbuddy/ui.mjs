// WorkBuddy relay status card (settings.extension slot).
// Static information only: the v1 host wires no data-plane actions, so the
// card never renders buttons — the credential and models live in the
// provider drawer on the Providers page.

import { define_plugin } from "zroutery:plugin/v1";

const COPY = {
  en: {
    notice:
      "Relays chat to the WorkBuddy (CodeBuddy) chat endpoint with the official CLI identity. The access token is stored in the system keyring and is only sent to www.workbuddy.cn.",
    endpoint: "Chat endpoint",
    auth: "Authentication",
    authValue: "Bearer <access token from the provider drawer>",
    steps:
      "1. Keep the relay (wb2api panel) or a WorkBuddy CLI login available for a valid access token. 2. Create the connection on the Providers page. 3. Save the access token in the provider drawer. 4. Add the models your account offers, then route traffic to them.",
  },
  "zh-CN": {
    notice:
      "以官方 CLI 身份把对话中继到 WorkBuddy（CodeBuddy）聊天端点。访问令牌存放在系统 keyring，只会发送给 www.workbuddy.cn。",
    endpoint: "对话端点",
    auth: "鉴权方式",
    authValue: "Bearer <提供方抽屉里保存的访问令牌>",
    steps:
      "1. 准备有效的访问令牌（WB2API 面板或 WorkBuddy CLI 登录）。2. 在「提供方」页面创建此连接。3. 在提供方抽屉里保存访问令牌。4. 添加账号可用的模型并接入分流。",
  },
  "zh-TW": {
    notice:
      "以官方 CLI 身分把對話中繼到 WorkBuddy（CodeBuddy）聊天端點。存取權杖存放在系統 keyring，只會傳送給 www.workbuddy.cn。",
    endpoint: "對話端點",
    auth: "鑑權方式",
    authValue: "Bearer <提供方抽屜裡保存的存取權杖>",
    steps:
      "1. 準備有效的存取權杖（WB2API 面板或 WorkBuddy CLI 登入）。2. 在「提供方」頁面建立此連線。3. 在提供方抽屜裡保存存取權杖。4. 新增帳號可用的模型並接入分流。",
  },
};

export default define_plugin({
  ui_render: {
    card: (input) => {
      const locale = COPY[input.locale] ?? COPY.en;
      return {
        kind: "stack",
        children: [
          { kind: "notice", tone: "info", text: locale.notice },
          {
            kind: "value",
            label: locale.endpoint,
            value: "POST https://www.workbuddy.cn/v2/chat/completions",
          },
          { kind: "value", label: locale.auth, value: locale.authValue },
          { kind: "text", text: locale.steps },
        ],
      };
    },
  },
});
