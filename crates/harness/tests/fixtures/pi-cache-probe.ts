// Load in an isolated Pi agent directory for pi_cache_notices.rs.
// A provider whose usage, cost and cache lifetime the test controls, so Pi's
// cache-miss and cache-warming logic runs for real without any network spend.
//
// Prompt protocol: `usage <input> <cacheRead> <cacheWrite> [slow]` makes the
// reply report exactly those prompt tokens. `slow` holds the reply for 1.5s.
import { appendFileSync } from "node:fs";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";

// Dollars per million tokens, Anthropic-shaped so the tests can check costs by hand.
const RATES = { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 };
let lastPromptTokens = 0;

const usageFor = (input, output, cacheRead, cacheWrite) => {
  const cost = {
    input: (input * RATES.input) / 1e6,
    output: (output * RATES.output) / 1e6,
    cacheRead: (cacheRead * RATES.cacheRead) / 1e6,
    cacheWrite: (cacheWrite * RATES.cacheWrite) / 1e6,
  };
  cost.total = cost.input + cost.output + cost.cacheRead + cost.cacheWrite;
  return { input, output, cacheRead, cacheWrite, totalTokens: input + output + cacheRead + cacheWrite, cost };
};

export default function (pi) {
  pi.registerProvider('zeron-cache', {
    baseUrl: 'http://127.0.0.1:1', apiKey: 'local-fake-key', api: 'zeron-cache-api',
    models: [{
      id: 'cached', name: 'Cached mock', reasoning: false, input: ['text'], cost: RATES,
      // 11s is the shortest lifetime Pi will warm: it refreshes after 1s.
      promptCache: { short: 11 }, contextWindow: 1000000, maxTokens: 4096,
    }],
    streamSimple(model, context, options) {
      const stream = createAssistantMessageEventStream();
      const last = [...context.messages].reverse().find(m => m.role === 'user');
      const text = typeof last?.content === 'string' ? last.content : (last?.content || []).filter(c => c.type === 'text').map(c => c.text).join('');
      const warm = options?.maxTokens === 1;
      const match = /^usage (\d+) (\d+) (\d+)( slow)?$/.exec(text);
      const [input, cacheRead, cacheWrite] = match ? match.slice(1, 4).map(Number) : [10, 0, 0];
      const usage = warm ? usageFor(0, 1, lastPromptTokens, 0) : usageFor(input, 50, cacheRead, cacheWrite);
      if (warm) {
        appendFileSync('probe-warm-calls.jsonl', JSON.stringify({ cacheRead: lastPromptTokens }) + '\n');
      } else {
        lastPromptTokens = input + cacheRead + cacheWrite;
      }
      const msg = { role: 'assistant', content: [], api: model.api, provider: model.provider, model: model.id, usage, stopReason: 'stop', timestamp: Date.now() };
      (async () => {
        stream.push({ type: 'start', partial: msg });
        const delay = warm ? 0 : match?.[4] ? 1500 : 20;
        await new Promise(resolve => {
          const t = setTimeout(resolve, delay);
          options?.signal?.addEventListener('abort', () => { clearTimeout(t); resolve(); }, { once: true });
        });
        if (options?.signal?.aborted) {
          msg.stopReason = 'aborted'; msg.errorMessage = 'aborted';
          stream.push({ type: 'error', reason: 'aborted', error: msg }); stream.end(); return;
        }
        msg.content.push({ type: 'text', text: '' });
        stream.push({ type: 'text_start', contentIndex: 0, partial: msg });
        msg.content[0].text = warm ? '.' : 'CACHED:' + text;
        stream.push({ type: 'text_delta', contentIndex: 0, delta: msg.content[0].text, partial: msg });
        stream.push({ type: 'text_end', contentIndex: 0, content: msg.content[0].text, partial: msg });
        stream.push({ type: 'done', reason: 'stop', message: msg }); stream.end();
      })();
      return stream;
    },
  });
}
