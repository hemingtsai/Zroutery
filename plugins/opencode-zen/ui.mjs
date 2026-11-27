// OpenCode Zen free-tier status card (settings.extension slot).
// Static information only: the v1 host wires no data-plane actions, so the
// card never renders buttons — models live on the Models page, and the
// session registration happens transparently per conversation.

const COPY = {
  en: {
    notice:
      "Relays chat to the OpenCode Zen anonymous free tier. Sessions are generated per conversation, registered through the model catalog before first use, and the request carries the OpenCode CLI identity with its environment block. No key, no login.",
    endpoint: "Chat endpoint",
    models: "Model catalog",
    gate: "How the gate is satisfied",
    gateValue: "Client-shaped headers + session registration + environment block",
    note: "The free tier is rate limited per IP; heavy parallel traffic may still see quota-flavored 429s that clear on their own.",
  },
  "zh-CN": {
    notice:
      "把对话中继到 OpenCode Zen 匿名免费层。会话按对话生成，首次使用前经模型目录登记，请求携带 OpenCode CLI 身份与环境块。无需密钥与登录。",
    endpoint: "对话端点",
    models: "模型目录",
    gate: "门禁满足方式",
    gateValue: "客户端整形头 + 会话登记 + 环境块",
    note: "免费层按 IP 限流；高并发时仍可能出现配额样 429，稍后自行恢复。",
  },
  "zh-TW": {
    notice:
      "把對話中繼到 OpenCode Zen 匿名免費層。會話按對話生成，首次使用前經模型目錄登記，請求攜帶 OpenCode CLI 身份與環境塊。無需密鑰與登錄。",
    endpoint: "對話端點",
    models: "模型目錄",
    gate: "門禁滿足方式",
    gateValue: "客戶端整形頭 + 會話登記 + 環境塊",
    note: "免費層按 IP 限流；高併發時仍可能出現配額樣 429，稍後自行恢復。",
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
            value: "POST https://opencode.ai/zen/v1/chat/completions",
          },
          { kind: "value", label: locale.models, value: "GET https://opencode.ai/zen/v1/models" },
          { kind: "value", label: locale.gate, value: locale.gateValue },
          { kind: "text", text: locale.note },
        ],
      };
    },
  },
});
