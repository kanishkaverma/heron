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
const ROOT = process.env.HERON_THREADS_DIR || path.join(HOME, ".heron", "threads");
const HERE = (() => {
  try {
    return path.dirname(new URL(import.meta.url).pathname);
  } catch {
    return path.join(HOME, "heron", ".pi", "extensions", "heron");
  }
})();
const RUNNER = path.join(HERE, "runner.mjs");
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

function allThreads() {
  let names = [];
  try {
    names = fs.readdirSync(ROOT);
  } catch {
    return [];
  }
  return names
    .map((id) => {
      const dir = path.join(ROOT, id);
      const spec = readJson(path.join(dir, "spec.json"), null);
      if (!spec) return null;
      return { id, dir, spec, status: readJson(path.join(dir, "status.json"), {}) };
    })
    .filter(Boolean)
    .sort((a, b) => (b.spec.createdAt || 0) - (a.spec.createdAt || 0));
}

function alive(pid) {
  if (!pid) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** A "running" thread whose runner died (machine restart, kill -9) is failed. */
function settleOrphans() {
  for (const t of allThreads()) {
    if ((t.status.state === "running" || t.status.state === "starting") && t.status.pid && !alive(t.status.pid)) {
      writeJson(path.join(t.dir, "status.json"), {
        ...t.status,
        state: "failed",
        error: "The thread's process ended unexpectedly (the computer may have restarted).",
        endedAt: Date.now(),
        pid: null,
        notified: false,
      });
    }
  }
}

// ---------------------------------------------------------------- helpers

function which(name, extra = []) {
  for (const dir of [...(process.env.PATH || "").split(":"), ...extra]) {
    if (!dir) continue;
    const p = path.join(dir, name);
    try {
      fs.accessSync(p, fs.constants.X_OK);
      return p;
    } catch {}
  }
  return null;
}

const COMMON = [path.join(HOME, ".local", "bin"), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

function piPath() {
  return process.env.PI_EXECUTABLE || which("pi", COMMON) || "pi";
}

function nodePath() {
  return process.env.HERON_NODE || which("node", [...COMMON, path.join(HOME, ".local/share/mise/installs/node/latest/bin")]) || "node";
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
You are a subagent. Don't run memo.

- Nobody reads anything from you until you finish, and nobody can answer questions: make sensible choices, note them, and keep going.
- Never send, post, publish, buy, book, pay, delete, accept or decline anything on the person's behalf. Prepare it instead (a draft, a filled form you didn't submit, a shortlist) and put it in your report.
- Use the person's connected accounts (the Executor tools: Gmail, Calendar, Drive, Slack, WhatsApp, …) to read what you need.
- Your last message is your report to Heron. Lead with the result in a sentence or two, then the details that matter (facts, links, drafts word for word, what you couldn't do and why). Plain words, no filler.`;

// ---------------------------------------------------------------- runs

function launch(dir) {
  const child = spawn(nodePath(), [RUNNER, dir], {
    detached: true,
    stdio: "ignore",
    cwd: dir,
    env: { ...process.env, HERON_PI: piPath() },
  });
  child.unref();
  return child.pid;
}

function startThread({ title, task, model }, parentSession) {
  fs.mkdirSync(ROOT, { recursive: true });
  const id = newId(title);
  const dir = path.join(ROOT, id);
  fs.mkdirSync(path.join(dir, "work"), { recursive: true });
  fs.mkdirSync(path.join(dir, "session"), { recursive: true });
  const now = Date.now();
  writeJson(path.join(dir, "spec.json"), { id, title, task, model: model || null, createdAt: now, parentSession: parentSession || null });
  fs.writeFileSync(path.join(dir, "brief.md"), BRIEF);
  writeJson(path.join(dir, "run.json"), { prompt: `You are a subagent. Don't run memo.\n\n${task}`, resume: false, at: now });
  writeJson(path.join(dir, "status.json"), { state: "starting", startedAt: now, runs: 1, notified: false });
  const pid = launch(dir);
  const status = readJson(path.join(dir, "status.json"), {});
  if (status.state === "starting") writeJson(path.join(dir, "status.json"), { ...status, pid });
  return id;
}

function continueThread(id, message) {
  const dir = threadDir(id);
  const status = readJson(path.join(dir, "status.json"), {});
  if ((status.state === "running" || status.state === "starting") && alive(status.pid)) {
    throw new Error(`Thread ${id} is still running. Wait for its report, or stop it with thread_stop first.`);
  }
  try {
    fs.rmSync(path.join(dir, "notified.lock"), { recursive: true, force: true });
  } catch {}
  const now = Date.now();
  writeJson(path.join(dir, "run.json"), { prompt: message, resume: true, at: now });
  writeJson(path.join(dir, "status.json"), { ...status, state: "starting", startedAt: now, endedAt: null, error: null, runs: (status.runs || 1) + 1, notified: false });
  const pid = launch(dir);
  const after = readJson(path.join(dir, "status.json"), {});
  if (after.state === "starting") writeJson(path.join(dir, "status.json"), { ...after, pid });
}

function stopThread(id) {
  const dir = threadDir(id);
  const status = readJson(path.join(dir, "status.json"), {});
  if (!alive(status.pid)) return false;
  // The runner leads its own process group (detached): stop it and its Pi.
  try {
    process.kill(-status.pid, "SIGTERM");
  } catch {
    try {
      process.kill(status.pid, "SIGTERM");
    } catch {}
  }
  return true;
}

/** The report the Heron agent gets when a thread ends. */
function report(t) {
  const s = t.status;
  const result = readText(path.join(t.dir, "result.md")).trim();
  const head = `[Heron thread finished] "${t.spec.title}" (${t.id}) — ${s.state}${s.startedAt ? ` after ${took(s.startedAt, s.endedAt)}` : ""}.`;
  const body = result
    ? result.length > REPORT_CHARS
      ? `${result.slice(0, REPORT_CHARS)}\n\n[… cut; thread_read ${t.id} has the whole report]`
      : result
    : "(The thread left no report.)";
  const err = s.state !== "done" && s.error ? `\nError: ${s.error}` : "";
  return `${head}${err}\n\nIts report:\n${body}\n\nThe person can't see threads: tell them what matters from this, in a few lines. Use thread_message to send this thread follow-up work.`;
}

/** Claim a finished thread's report exactly once (several Pi processes may watch). */
function claim(t) {
  try {
    fs.mkdirSync(path.join(t.dir, "notified.lock"));
  } catch {
    return false;
  }
  writeJson(path.join(t.dir, "status.json"), { ...readJson(path.join(t.dir, "status.json"), {}), notified: true, notifiedAt: Date.now() });
  return true;
}

function finishedUnreported(sessionFile) {
  settleOrphans();
  return allThreads().filter((t) => {
    if (!["done", "failed", "stopped"].includes(t.status.state) || t.status.notified) return false;
    if (fs.existsSync(path.join(t.dir, "notified.lock"))) return false;
    // A thread belongs to the chat that started it (another chat in this
    // folder doesn't get its report), unless that chat's session is gone.
    const parent = t.spec.parentSession;
    return !parent || !sessionFile || parent === sessionFile || !fs.existsSync(parent);
  });
}

// ---------------------------------------------------------------- extension

export default function heron(pi) {
  let timer = null;
  let sessionFile = null;
  let ctxRef = null;
  /** Reports found when this process started: they ride along with the next run. */
  let carried = [];

  const sessionOf = (ctx) => {
    try {
      return ctx?.sessionManager?.getSessionFile?.() || null;
    } catch {
      return null;
    }
  };

  function tick() {
    const ctx = ctxRef;
    sessionFile = sessionOf(ctx) || sessionFile;
    const ready = finishedUnreported(sessionFile).filter((t) => !carried.includes(t.id));
    for (const t of ready) {
      if (!claim(t)) continue;
      const idle = typeof ctx?.isIdle === "function" ? ctx.isIdle() : true;
      pi.sendMessage(
        { customType: "heron-thread", content: report({ ...t, status: readJson(path.join(t.dir, "status.json"), t.status) }), display: false, details: { thread: t.id } },
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
    sessionFile = sessionOf(ctx);
    // Reports that finished while no Heron process was up go out with the
    // next run (most likely the message that brought this process up).
    carried = finishedUnreported(sessionFile).map((t) => t.id);
    startWatching();
  });

  pi.on("session_shutdown", async () => {
    if (timer) clearInterval(timer);
    timer = null;
  });

  pi.on("before_agent_start", async (event, ctx) => {
    ctxRef = ctx;
    if (!carried.length) return;
    const ids = carried;
    carried = [];
    const reports = [];
    for (const id of ids) {
      const t = allThreads().find((x) => x.id === id);
      if (t && claim(t)) reports.push(report({ ...t, status: readJson(path.join(t.dir, "status.json"), t.status) }));
    }
    if (!reports.length) return;
    return {
      message: {
        customType: "heron-thread",
        content: `While you were away, ${reports.length === 1 ? "a background thread" : `${reports.length} background threads`} finished:\n\n${reports.join("\n\n---\n\n")}`,
        display: false,
      },
    };
  });

  pi.registerTool({
    name: "thread_start",
    label: "Start background thread",
    description:
      "Hand a task to a background thread: a separate agent on this computer, with the same tools and accounts (Gmail, Calendar, web, files, …), that works on its own and reports back. Returns at once. When it finishes you get a message starting \"[Heron thread finished]\" with its report, and you're woken to pass on what matters. The person never sees threads. The thread does not see this chat or your memory: put everything it needs in `task` (who, what, where to look, what to hand back, constraints). Threads never send or buy anything; they prepare it for you.",
    promptSnippet: "Start a background thread (a separate agent) on a task; it reports back when done",
    promptGuidelines: [
      "Use thread_start for anything that takes more than a minute or two (research, inbox or calendar triage, comparing options, long browsing, several independent jobs at once), then tell the person in a line what's underway and end your turn.",
      "Write thread_start tasks as complete briefs: the thread can't see this chat or your memory.",
      "When a \"[Heron thread finished]\" message arrives, tell the person the outcome in a few lines; they can't see threads.",
    ],
    parameters: {
      type: "object",
      properties: {
        title: { type: "string", description: "A few words naming the job, e.g. \"Find Vancouver dentists\"." },
        task: { type: "string", description: "The full brief: the goal, everything the thread needs to know, where to look, and exactly what to hand back." },
        model: { type: "string", description: "Optional model id (provider/id) for this thread; omit for the default." },
      },
      required: ["title", "task"],
    },
    async execute(_id, params, _signal, _onUpdate, ctx) {
      const id = startThread(params, sessionOf(ctx) || sessionFile);
      startWatching();
      return text(`Started background thread ${id} ("${params.title}"). It works on its own; its report will arrive as a "[Heron thread finished]" message. Tell the person in a line that it's underway, then end your turn.`);
    },
  });

  pi.registerTool({
    name: "thread_list",
    label: "List background threads",
    description: "Your background threads, newest first: id, title, state (starting, running, done, failed, stopped) and timing.",
    parameters: { type: "object", properties: { limit: { type: "number", description: "How many (default 15)." } } },
    async execute(_id, params) {
      settleOrphans();
      const list = allThreads().slice(0, params?.limit || 15);
      if (!list.length) return text("No background threads yet.");
      return text(
        list
          .map((t) => {
            const s = t.status;
            const when = s.state === "running" || s.state === "starting" ? `running for ${took(s.startedAt)}` : `${s.state} ${ago(s.endedAt || t.spec.createdAt)} (took ${took(s.startedAt, s.endedAt)})`;
            return `- ${t.id} — "${t.spec.title}" — ${when}${s.runs > 1 ? `, ${s.runs} runs` : ""}`;
          })
          .join("\n"),
      );
    },
  });

  pi.registerTool({
    name: "thread_read",
    label: "Read a background thread",
    description: "A thread's report if it finished, or its progress so far if it's running (tool calls made, latest step, latest text).",
    parameters: { type: "object", properties: { id: { type: "string", description: "Thread id (from thread_start or thread_list)." } }, required: ["id"] },
    async execute(_id, params) {
      const dir = threadDir(params.id);
      const spec = readJson(path.join(dir, "spec.json"), {});
      const s = readJson(path.join(dir, "status.json"), {});
      const lines = [`"${spec.title}" (${params.id}): ${s.state}${s.startedAt ? `, ${took(s.startedAt, s.endedAt)}` : ""}${s.error ? `\nError: ${s.error}` : ""}`];
      if (s.state === "running" || s.state === "starting") {
        const p = readJson(path.join(dir, "progress.json"), {});
        lines.push(`Tool calls so far: ${p.tools || 0}${p.lastTool ? `; latest: ${p.lastTool}` : ""}`);
        if (p.lastText) lines.push(`Latest text:\n${p.lastText}`);
      } else {
        lines.push(`Report:\n${readText(path.join(dir, "result.md")).trim() || "(none)"}`);
      }
      return text(lines.join("\n"));
    },
  });

  pi.registerTool({
    name: "thread_message",
    label: "Send a background thread follow-up work",
    description: "Give a finished thread follow-up work (it keeps its own conversation, so it remembers what it did). Runs in the background and reports back like thread_start.",
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
      return text(`Thread ${params.id} is working on the follow-up; its report will arrive as a "[Heron thread finished]" message.`);
    },
  });

  pi.registerTool({
    name: "thread_stop",
    label: "Stop a background thread",
    description: "Stop a running thread. Its partial report still comes back.",
    parameters: { type: "object", properties: { id: { type: "string", description: "Thread id." } }, required: ["id"] },
    async execute(_id, params) {
      return text(stopThread(params.id) ? `Stopping ${params.id}.` : `${params.id} isn't running.`);
    },
  });
}
