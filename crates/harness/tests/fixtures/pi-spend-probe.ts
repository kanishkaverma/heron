import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
const PRICE = { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 };
const USAGE = {
  warm: { input: 200, cacheRead: 0, cacheWrite: 30000 },
  hit: { input: 200, cacheRead: 30000, cacheWrite: 0 },
  miss: { input: 30200, cacheRead: 0, cacheWrite: 0 },
  idle: { input: 200, cacheRead: 0, cacheWrite: 30000 },
};
export default function (pi) {
  pi.registerProvider("zeron-spend", {
    baseUrl: "http://127.0.0.1:1", apiKey: "local-fake-key", api: "zeron-spend-api",
    models: [{ id: "priced", name: "Priced mock", reasoning: false, input: ["text"], cost: PRICE, contextWindow: 200000, maxTokens: 4096 }],
    streamSimple(model, context) {
      const stream = createAssistantMessageEventStream();
      const last = [...context.messages].reverse().find((m) => m.role === "user");
      const text = typeof last?.content === "string" ? last.content : (last?.content || []).filter((c) => c.type === "text").map((c) => c.text).join("");
      const u = { ...(USAGE[text] ?? USAGE.hit), output: 500 };
      const cost = Object.fromEntries(Object.entries(PRICE).map(([k, rate]) => [k, (u[k] * rate) / 1e6]));
      cost.total = cost.input + cost.output + cost.cacheRead + cost.cacheWrite;
      const timestamp = Date.now() + (text === "idle" ? 7 * 60_000 : 0);
      const msg = { role: "assistant", content: [], api: model.api, provider: model.provider, model: model.id,
        usage: { ...u, totalTokens: u.input + u.output + u.cacheRead + u.cacheWrite, cost }, stopReason: "stop", timestamp };
      (async () => {
        stream.push({ type: "start", partial: msg });
        msg.content.push({ type: "text", text: "" });
        stream.push({ type: "text_start", contentIndex: 0, partial: msg });
        msg.content[0].text = "MOCK:" + text;
        stream.push({ type: "text_delta", contentIndex: 0, delta: msg.content[0].text, partial: msg });
        stream.push({ type: "text_end", contentIndex: 0, content: msg.content[0].text, partial: msg });
        stream.push({ type: "done", reason: "stop", message: msg });
        stream.end();
      })();
      return stream;
    },
  });
}
