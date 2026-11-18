// @ts-nocheck
// Heron: background threads for the Heron chat.
//
// The Heron chat's agent hands longer work to background threads: separate Pi
// runs on this computer that the person never sees or talks to. Each thread
// runs detached (it outlives this Pi process), keeps its own session, and
// when it ends its report comes back here as a message that wakes the agent,
// which tells the person what matters.
//
// Files: ~/.heron/threads/<id>/{spec.json,status.json,run.json,brief.md,
// result.md,progress.json,events.jsonl,stderr.log,session/,work/}
// ($HERON_THREADS_DIR overrides the root).

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const HOME = os.homedir();
// Heron's memory: its own OptMem store, synced across the Heron computers
// (Syncthing), separate from any other OptMem memory on the machine. Every
// command the agent runs inherits this.
process.env.MEMORY_DIR = process.env.HERON_MEMORY_DIR || path.join(HOME, ".heron", "memory");
const ROOT = process.env.HERON_THREADS_DIR || path.join(HOME, ".heron", "threads");
const HERE = (() => {
  try {
    return path.dirname(new URL(import.meta.url).pathname);
  } catch {
    return path.join(HOME, "heron", ".pi", "extensions", "heron");
  }
})();
const POLL_MS = 3000;
const REPORT_CHARS = 8000;

// ---------------------------------------------------------------- files

function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return fallback;
  }
}

function writeJson(file, value) {
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2));
  fs.renameSync(tmp, file);
}

function readText(file, fallback = "") {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return fallback;
  }
}

function threadDir(id) {
  if (!/^[a-z0-9-]+$/i.test(id)) throw new Error(`No thread "${id}"`);
  const dir = path.join(ROOT, id);
  if (!fs.existsSync(path.join(dir, "spec.json"))) throw new Error(`No thread "${id}" (see thread_list)`);
  return dir;
}

/** One task as the files say: what was asked (request), who has it (lease), how it's going (status). */
function load(id, dir = path.join(ROOT, id)) {
  const spec = readJson(path.join(dir, "spec.json"), null);
  if (!spec) return null;
  const request = readJson(path.join(dir, "request.json"), { req: 1 });
  return {
    id,
    dir,
    spec,
    request,
    lease: readJson(path.join(dir, "lease.json"), {}),
    status: readJson(path.join(dir, "status.json"), {}),
    stop: readJson(path.join(dir, "stop.json"), {}),
  };
}

function allThreads() {
  let names = [];
  try {
    names = fs.readdirSync(ROOT);
  } catch {
    return [];
  }
  return names
    .map((id) => load(id))
    .filter(Boolean)
    .sort((a, b) => (b.spec.createdAt || 0) - (a.spec.createdAt || 0));
}

const FINAL = new Set(["done", "failed", "stopped"]);

/** Where the current request stands, in words. */
function stateOf(t) {
  const req = t.request.req;
  const st = t.status;
  if (st.req === req && FINAL.has(st.state)) return st.state;
  if (t.stop.req === req) return "stopping";
  if (st.req === req && st.state === "running") return "running";
  if (t.lease.req === req) return "starting";
  return "queued";
}

function newId(title) {
  const slug = String(title || "thread")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 24) || "thread";
  return `${slug}-${Math.random().toString(36).slice(2, 6)}`;
}

function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 90) return `${s}s ago`;
  if (s < 5400) return `${Math.round(s / 60)}m ago`;
  if (s < 129600) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function took(a, b) {
  if (!a) return "";
  const s = Math.max(0, Math.round(((b || Date.now()) - a) / 1000));
  return s < 90 ? `${s}s` : s < 5400 ? `${Math.round(s / 60)}m` : `${(s / 3600).toFixed(1)}h`;
}

function text(t) {
  return { content: [{ type: "text", text: t }], details: {} };
}

const BRIEF = `You are a background thread working for Heron, a personal assistant, on one task you were handed.

- Nobody reads anything from you until you finish, and nobody can answer questions: make sensible choices, note them, and keep going.
- Never send, post, publish, buy, book, pay, delete, accept or decline anything on the person's behalf. Prepare it instead (a draft, a filled form you didn't submit, a shortlist) and put it in your report.
- Use the person's connected accounts (the Executor tools: Gmail, Calendar, Drive, Slack, WhatsApp, …) to read what you need.
- Keep files you make in your working folder: if this computer goes down, another one continues from them. Build outputs (node_modules, target/) there are not synced.
- Ending your turn ends the task. If you started something that reports later (a remote chat, a background job), wait for it (wait=true, a Monitor, or polling) before you write your report; never end on "waiting for results".
- Your last message is your report to Heron. Lead with the result in a sentence or two, then the details that matter (facts, links, drafts word for word, what you couldn't do and why). Plain words, no filler.`;

// ---------------------------------------------------------------- tasks
//
// A task is a folder in ~/.heron/threads, the same on every Heron computer
// (Syncthing). This chat writes what's asked (spec, request, stop) and marks
// reports as delivered (notified-<req>); heron-keeper leases the task to an
// online computer and runs it there; if that computer goes down, it moves to
// another one and resumes from the log. See runner.mjs for who writes what.

function chatId() {
  try {
    return JSON.parse(process.env.ZERON_PI_MCP || "{}")?.env?.ZERON_CHAT_ID || null;
  } catch {
    return null;
  }
}

function startThread({ title, task, model }) {
  fs.mkdirSync(ROOT, { recursive: true });
  const id = newId(title);
  const dir = path.join(ROOT, id);
  fs.mkdirSync(path.join(dir, "work"), { recursive: true });
  const now = Date.now();
  fs.writeFileSync(path.join(dir, "brief.md"), BRIEF);
  writeJson(path.join(dir, "spec.json"), { id, title, task, model: model || null, createdAt: now, chat: chatId(), from: os.hostname() });
  writeJson(path.join(dir, "request.json"), { req: 1, mode: "fresh", prompt: task, at: now });
  return id;
}

function continueThread(id, message) {
  const t = load(id, threadDir(id));
  if (!FINAL.has(stateOf(t))) {
    throw new Error(`Thread ${id} is still ${stateOf(t)}. Wait for its report, or stop it with thread_stop first.`);
  }
  writeJson(path.join(t.dir, "request.json"), { req: t.request.req + 1, mode: "followup", prompt: message, at: Date.now() });
}

function stopThread(id) {
  const t = load(id, threadDir(id));
  const state = stateOf(t);
  if (FINAL.has(state)) return false;
  writeJson(path.join(t.dir, "stop.json"), { req: t.request.req, at: Date.now() });
  // Not handed to a computer yet: nobody else will close it.
  if (t.lease.req !== t.request.req) {
    writeJson(path.join(t.dir, "status.json"), { state: "stopped", req: t.request.req, endedAt: Date.now() });
  }
  return true;
}

/** The report the Heron agent gets when a task ends. */
function report(t) {
  const s = t.status;
  const req = t.request.req;
  const result = (readText(path.join(t.dir, `result-${req}.md`)) || readText(path.join(t.dir, "result.md"))).trim();
  const where = s.host ? ` on ${s.host}` : "";
  const moved = (s.attempt || 1) > 1 ? ` (moved computers ${s.attempt - 1}×)` : "";
  const head = `[Heron thread finished] "${t.spec.title}" (${t.id}) — ${s.state}${where}${moved}${s.startedAt ? ` after ${took(s.startedAt, s.endedAt)}` : ""}.`;
  const body = result
    ? result.length > REPORT_CHARS
      ? `${result.slice(0, REPORT_CHARS)}\n\n[… cut; thread_read ${t.id} has the whole report]`
      : result
    : "(The thread left no report.)";
  const err = s.state !== "done" && s.error ? `\nError: ${s.error}` : "";
  return `${head}${err}\n\nIts report:\n${body}\n\nThe person can't see threads: tell them what matters from this, in a few lines. Use thread_message to send this thread follow-up work.`;
}

/**
 * A report is delivered in two steps: `delivering-<req>` when it's handed to
 * the agent, `notified-<req>` once the reply that carries it has finished
 * (see agent_settled). A delivery whose agent died (its computer went down
 * mid-reply) goes stale and is handed over again, here or elsewhere.
 */
const DELIVERY_STALE_MS = 180000;

function deliveringFresh(t) {
  const d = readJson(path.join(t.dir, `delivering-${t.request.req}`), null);
  return Boolean(d && Date.now() - (d.at || 0) < DELIVERY_STALE_MS);
}

function claim(t) {
  if (fs.existsSync(path.join(t.dir, `notified-${t.request.req}`)) || deliveringFresh(t)) return false;
  writeJson(path.join(t.dir, `delivering-${t.request.req}`), { host: os.hostname(), pid: process.pid, at: Date.now() });
  return true;
}

function markDelivered(t) {
  try {
    fs.writeFileSync(path.join(t.dir, `notified-${t.request.req}`), new Date().toISOString());
  } catch {}
}

function finishedUnreported() {
  const mine = chatId();
  return allThreads().filter((t) => {
    if (!FINAL.has(stateOf(t))) return false;
    if (fs.existsSync(path.join(t.dir, `notified-${t.request.req}`))) return false;
    if (deliveringFresh(t)) return false;
    // A task belongs to the chat that started it.
    return !t.spec.chat || !mine || t.spec.chat === mine;
  });
}

// ---------------------------------------------------------------- catch-up
//
// The Heron chat can move between computers (heron-keeper takes it over when
// its computer goes offline). The new computer's Pi starts a fresh session,
// so the first run there reads the chat's earlier messages from Zeron (every
// computer has the synced transcript) and gets them as context.

const CATCHUP_MESSAGES = 40;
const CATCHUP_CHARS = 16000;

function zeronMcp() {
  try {
    const config = JSON.parse(process.env.ZERON_PI_MCP || "");
    return config?.command ? config : null;
  } catch {
    return null;
  }
}

/** One Zeron MCP tool call through a short-lived `zeron mcp`. */
function zeronTool(config, name, args, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    const child = spawn(config.command, config.args || [], {
      env: { ...process.env, ...(config.env || {}) },
      stdio: ["pipe", "pipe", "ignore"],
    });
    let buf = "";
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error("zeron mcp timed out"));
    }, timeoutMs);
    const send = (msg) => child.stdin.write(JSON.stringify(msg) + "\n");
    child.stdout.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        let msg;
        try {
          msg = JSON.parse(line);
        } catch {
          continue;
        }
        if (msg.id === 1) {
          send({ jsonrpc: "2.0", method: "notifications/initialized" });
          send({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name, arguments: args } });
        } else if (msg.id === 2) {
          clearTimeout(timer);
          child.kill();
          if (msg.error) return reject(new Error(JSON.stringify(msg.error)));
          const text = (msg.result?.content || []).filter((c) => c.type === "text").map((c) => c.text).join("");
          try {
            resolve(JSON.parse(text));
          } catch {
            resolve(text);
          }
        }
      }
    });
    child.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "heron", version: "1" } } });
  });
}

/** The chat's earlier messages as context, or null when there are none. */
async function catchUp(prompt, sinceMs = 0) {
  const config = zeronMcp();
  const chat = config?.env?.ZERON_CHAT_ID;
  if (!chat) return null;
  const read = await zeronTool(config, "read_chat", { chat, limit: CATCHUP_MESSAGES, include_tools: false });
  // Only what this session hasn't seen: messages after its last entry
  // (all of them for a fresh session).
  const messages = (read?.messages || []).filter((m) => (m.text || "").trim() && (m.createdAt || 0) > sinceMs);
  // The newest user message is the one this run is answering.
  const last = messages[messages.length - 1];
  if (last && last.role === "user" && (!prompt || prompt.includes(last.text.trim().slice(0, 200)) || last.text.includes(String(prompt).trim().slice(0, 200)))) {
    messages.pop();
  }
  if (!messages.length) return null;
  const lines = [];
  let total = 0;
  for (const m of messages.slice().reverse()) {
    const who = m.role === "user" ? "Person" : "Heron (you)";
    let t = m.text.trim();
    if (t.length > 1500) t = `${t.slice(0, 1500)} […]`;
    const line = `${who} (${m.createdAtIso || ""}): ${t}`;
    if (total + line.length > CATCHUP_CHARS) break;
    lines.unshift(line);
    total += line.length;
  }
  const how = sinceMs ? "your session here is missing its latest messages" : "your session here started fresh";
  return `[Heron catch-up] This chat ran on another computer and has moved to this one (${os.hostname()}); ${how}. These are the messages you haven't seen, oldest first, from the chat's synced transcript. Carry on from them; don't mention the move unless asked.\n\n${lines.join("\n\n")}`;
}

// ---------------------------------------------------------------- extension

export default function heron(pi) {
  let timer = null;
  let ctxRef = null;
  /** Reports found when this process started: they ride along with the next run. */
  let carried = [];
  /** First run of this process: catch up on messages from other computers. */
  let catchUpPending = false;
  /** When this session last saw a message (0: never). */
  let sessionSeenMs = 0;
  /** Messages from other computers, kept in the prompt (see before_agent_start). */
  let catchUpSection = null;
  /** Reports handed to the agent whose reply hasn't finished yet. */
  let inFlight = [];

  function tick() {
    const ctx = ctxRef;
    const ready = finishedUnreported().filter((t) => !carried.includes(t.id));
    for (const t of ready) {
      if (!claim(t)) continue;
      inFlight.push(t);
      const idle = typeof ctx?.isIdle === "function" ? ctx.isIdle() : true;
      pi.sendMessage(
        { customType: "heron-thread", content: report(t), display: false, details: { thread: t.id } },
        { triggerTurn: true, deliverAs: idle ? "steer" : "followUp" },
      );
    }
  }

  function startWatching() {
    if (timer) return;
    timer = setInterval(() => {
      try {
        tick();
      } catch (e) {
        // Never let a bad file take the chat down.
      }
    }, POLL_MS);
    timer.unref?.();
  }

  pi.on("session_start", async (_event, ctx) => {
    ctxRef = ctx;
    // Reports that finished while no Heron process was up go out with the
    // next run (most likely the message that brought this process up).
    carried = finishedUnreported().map((t) => t.id);
    try {
      const entries = ctx.sessionManager?.getEntries?.() || [];
      sessionSeenMs = entries
        .filter((e) => e.type === "message")
        .reduce((max, e) => Math.max(max, Date.parse(e.timestamp || "") || 0), 0);
      const saved = entries.filter((e) => e.type === "custom" && e.customType === "heron-catchup").pop();
      catchUpSection = saved?.data?.text || null;
      catchUpPending = Boolean(zeronMcp()?.env?.ZERON_CHAT_ID);
    } catch {
      catchUpPending = false;
    }
    startWatching();
  });

  // The reply that carried reports finished: they're delivered.
  pi.on("agent_settled", async (event) => {
    if (event?.aborted || !inFlight.length) return;
    for (const t of inFlight) markDelivered(t);
    inFlight = [];
  });

  pi.on("session_shutdown", async () => {
    if (timer) clearInterval(timer);
    timer = null;
  });

  pi.on("before_agent_start", async (event, ctx) => {
    ctxRef = ctx;
    // Catch-up rides in a prompt section (an extra message here would
    // scramble the host's transcript), saved in the session so a restarted
    // process keeps it.
    if (catchUpPending && event.messageType !== "custom") {
      catchUpPending = false;
      try {
        const fresh = await catchUp(event.prompt, sessionSeenMs);
        if (fresh) {
          catchUpSection = (catchUpSection ? `${catchUpSection}\n\n${fresh}` : fresh).slice(-CATCHUP_CHARS * 2);
          pi.appendEntry("heron-catchup", { text: catchUpSection });
        }
      } catch {}
    }
    if (catchUpSection && event.systemPromptOptions?.sections) {
      event.systemPromptOptions.sections.heron_catchup = catchUpSection;
    }
    // Reports that finished while no Heron process was up (the chat moved
    // here, or heron-keeper's nudge started this run): handed over in a
    // prompt section for this run only.
    const sections = event.systemPromptOptions?.sections;
    if (sections && "heron_reports" in sections) delete sections.heron_reports;
    const reports = [];
    for (const id of carried) {
      const t = allThreads().find((x) => x.id === id);
      if (t && claim(t)) {
        reports.push(report(t));
        inFlight.push(t);
      }
    }
    carried = [];
    if (reports.length && sections) {
      sections.heron_reports = `${reports.length === 1 ? "A background thread" : `${reports.length} background threads`} finished while you weren't running. Tell the person what matters from ${reports.length === 1 ? "it" : "each"} now, in this reply:\n\n${reports.join("\n\n---\n\n")}`;
    }
  });

  pi.registerTool({
    name: "thread_start",
    label: "Start background thread",
    description:
      "Hand a task to a background thread: a separate agent, run on whichever of the person's computers is least busy, with the same tools and accounts (Gmail, Calendar, web, files, …). It works on its own and reports back; if its computer goes offline it continues on another one. Returns at once. When it finishes you get a message starting \"[Heron thread finished]\" with its report, and you're woken to pass on what matters. Start several for independent jobs: they run in parallel. The person never sees threads. A thread does not see this chat or your memory: put everything it needs in `task` (who, what, where to look, what to hand back, constraints). Threads never send or buy anything; they prepare it for you.",
    promptSnippet: "Start a background thread (a separate agent on any of the person's computers); it reports back when done",
    promptGuidelines: [
      "Use thread_start for anything that takes more than a minute or so (research, inbox or calendar triage, comparing options, long browsing, anything multi-step), so this chat stays free for the person's next request; start one thread per independent job.",
      "Write thread_start tasks as complete briefs: the thread can't see this chat or your memory.",
      "When a \"[Heron thread finished]\" message arrives, tell the person the outcome in a few lines; they can't see threads.",
    ],
    parameters: {
      type: "object",
      properties: {
        title: { type: "string", description: "A few words naming the job, e.g. \"Find Vancouver dentists\"." },
        task: { type: "string", description: "The full brief: the goal, everything the thread needs to know, where to look, and exactly what to hand back." },
        model: { type: "string", description: "Optional model id (provider/id); omit for each computer's Heron model." },
      },
      required: ["title", "task"],
    },
    async execute(_id, params) {
      const id = startThread(params);
      startWatching();
      return text(`Started background thread ${id} ("${params.title}"); a free computer picks it up within seconds. Its report will arrive as a "[Heron thread finished]" message. Tell the person in a line that it's underway, then end your turn.`);
    },
  });

  pi.registerTool({
    name: "thread_list",
    label: "List background threads",
    description: "Your background threads, newest first: id, title, state (queued, starting, running, done, failed, stopped), the computer running it, and timing.",
    parameters: { type: "object", properties: { limit: { type: "number", description: "How many (default 15)." } } },
    async execute(_id, params) {
      const list = allThreads().slice(0, params?.limit || 15);
      if (!list.length) return text("No background threads yet.");
      return text(
        list
          .map((t) => {
            const s = t.status;
            const state = stateOf(t);
            const where = s.host || t.lease.ownerName || "";
            const when = FINAL.has(state)
              ? `${state} ${ago(s.endedAt || t.spec.createdAt)} (took ${took(s.startedAt, s.endedAt)})`
              : state === "running"
                ? `running for ${took(s.startedAt)}`
                : state;
            const moved = (t.lease.attempt || 1) > 1 ? `, moved ${t.lease.attempt - 1}×` : "";
            return `- ${t.id} — "${t.spec.title}" — ${when}${where ? ` on ${where}` : ""}${moved}${t.request.req > 1 ? `, follow-up ${t.request.req - 1}` : ""}`;
          })
          .join("\n"),
      );
    },
  });

  pi.registerTool({
    name: "thread_read",
    label: "Read a background thread",
    description: "A thread's report if it finished, or its progress so far if it's running (computer, tool calls made, latest step, latest text).",
    parameters: { type: "object", properties: { id: { type: "string", description: "Thread id (from thread_start or thread_list)." } }, required: ["id"] },
    async execute(_id, params) {
      const t = load(params.id, threadDir(params.id));
      const s = t.status;
      const state = stateOf(t);
      const lines = [`"${t.spec.title}" (${t.id}): ${state}${s.host ? ` on ${s.host}` : ""}${s.startedAt ? `, ${took(s.startedAt, s.endedAt)}` : ""}${s.error ? `\nError: ${s.error}` : ""}`];
      if (FINAL.has(state)) {
        lines.push(`Report:\n${(readText(path.join(t.dir, `result-${t.request.req}.md`)) || readText(path.join(t.dir, "result.md"))).trim() || "(none)"}`);
      } else {
        const p = readJson(path.join(t.dir, "progress.json"), {});
        lines.push(`Tool calls so far: ${p.tools || 0}${p.lastTool ? `; latest: ${p.lastTool}` : ""}`);
        if (p.lastText) lines.push(`Latest text:\n${p.lastText}`);
      }
      return text(lines.join("\n"));
    },
  });

  pi.registerTool({
    name: "thread_message",
    label: "Send a background thread follow-up work",
    description: "Give a finished thread follow-up work (it gets its earlier task and report as context). Runs in the background on any free computer and reports back like thread_start.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Thread id." },
        message: { type: "string", description: "The follow-up instruction." },
      },
      required: ["id", "message"],
    },
    async execute(_id, params) {
      continueThread(params.id, params.message);
      startWatching();
      return text(`Thread ${params.id} has the follow-up; its report will arrive as a "[Heron thread finished]" message.`);
    },
  });

  pi.registerTool({
    name: "thread_stop",
    label: "Stop a background thread",
    description: "Stop a queued or running thread, wherever it runs. Its partial report still comes back.",
    parameters: { type: "object", properties: { id: { type: "string", description: "Thread id." } }, required: ["id"] },
    async execute(_id, params) {
      return text(stopThread(params.id) ? `Stopping ${params.id}.` : `${params.id} isn't running.`);
    },
  });
}
