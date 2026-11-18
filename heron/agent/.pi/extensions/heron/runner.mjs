#!/usr/bin/env node
// Runs one attempt of one Heron background task, detached from everything:
//   node runner.mjs <task dir> <req> <attempt> [--resume]
//
// Started by heron-keeper on the computer the task is leased to. A task dir
// lives in ~/.heron/threads, which Syncthing keeps the same on every Heron
// computer, so who writes what is fixed:
//   spec.json, request.json, stop.json, notified-<req>   the Heron chat
//   lease.json                                           the leader keeper
//   status.json, log.md, progress.json, result*.md, work/ this runner (the owner)
// session/, events.jsonl and stderr.log stay on this computer.
//
// --resume: an earlier attempt was cut off (its computer went down, or the
// run died). The prompt carries that attempt's log so this one continues.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [dir, reqArg, attemptArg] = process.argv.slice(2);
const resume = process.argv.includes("--resume");
if (!dir) {
  console.error("usage: runner.mjs <task dir> <req> <attempt> [--resume]");
  process.exit(2);
}
const HOME = os.homedir();
const file = (f) => path.join(dir, f);
const read = (f, fallback) => {
  try {
    return JSON.parse(fs.readFileSync(file(f), "utf8"));
  } catch {
    return fallback;
  }
};
const readText = (f) => {
  try {
    return fs.readFileSync(file(f), "utf8");
  } catch {
    return "";
  }
};
const write = (f, value) => {
  const tmp = `${file(f)}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2));
  fs.renameSync(tmp, file(f));
};
const logLine = (line) => fs.appendFileSync(file("log.md"), `${line}\n`);

const spec = read("spec.json", {});
const request = read("request.json", {});
const req = Number(reqArg || request.req || 1);
const attempt = Number(attemptArg || 1);
const host = os.hostname().replace(/\.local$/, "");
const startedAt = Date.now();
const previous = read("status.json", {});

write("status.json", {
  state: "running",
  req,
  attempt,
  host,
  pid: process.pid,
  startedAt,
  restarts: previous.req === req && previous.attempt === attempt ? (previous.restarts || 0) + (resume ? 1 : 0) : 0,
});
logLine(`\n## ${new Date(startedAt).toISOString()} · request ${req}, attempt ${attempt} on ${host}${resume ? " (resuming)" : ""}`);

// ---------------------------------------------------------------- the prompt

const clip = (s, n) => (s.length > n ? `…${s.slice(-n)}` : s);
let prompt;
if (request.mode === "followup") {
  const before = readText(`result-${req - 1}.md`) || readText("result.md");
  prompt = `You are a subagent. Don't run memo.\n\nEarlier you worked on this task:\n${spec.task}\n\nYour report then was:\n${clip(before, 6000) || "(none)"}\n\nNow do this follow-up:\n${request.prompt}`;
} else {
  prompt = `You are a subagent. Don't run memo.\n\n${request.prompt || spec.task}`;
}
if (resume) {
  prompt += `\n\n---\nThis task was already under way on another run that was cut off (its computer went offline, or the run stopped). Its log so far, newest last:\n${clip(readText("log.md"), 8000)}\n\nContinue from where it stopped: don't redo steps it finished; files it made are in your working folder.`;
}

// ---------------------------------------------------------------- the models
//
// ~/.heron/models.json lists the models that work on this computer, best
// first (setup-computer.sh probes them). A run that fails on a limit or a
// login error is retried on the next one.

function heronModels() {
  if (spec.model) return [spec.model];
  try {
    const list = JSON.parse(fs.readFileSync(path.join(HOME, ".heron", "models.json"), "utf8"));
    if (Array.isArray(list) && list.length) return list;
  } catch {}
  try {
    const s = JSON.parse(fs.readFileSync(path.join(HOME, "heron", ".pi", "settings.json"), "utf8"));
    if (s.defaultProvider && s.defaultModel) return [`${s.defaultProvider}/${s.defaultModel}`];
  } catch {}
  return [null];
}
const MODEL_TROUBLE = /rate.?limit|session limit|usage limit|quota|429|401|403|authenticat|expired|overloaded|subscription/i;

// The chat's Zeron identity stays with the chat.
const env = { ...process.env, HERON_THREAD_ID: spec.id || path.basename(dir) };
for (const key of Object.keys(env)) if (key.startsWith("ZERON_")) delete env[key];

const pi = process.env.HERON_PI || "pi";
fs.mkdirSync(file("work"), { recursive: true });
const events = fs.createWriteStream(file("events.jsonl"), { flags: "a" });
const errors = fs.createWriteStream(file("stderr.log"), { flags: "a" });

let child = null;
let lastText = "";
let turnText = ""; // what the agent said last in the current turn
let errorText = "";

// ---------------------------------------------------------------- the report
//
// The report is what the agent said at the end of a turn. A later turn (a
// Monitor's late notice, a nudge from the runner) usually ends with a short
// note like "that notice changes nothing": it must not replace the report.
// Such a note is appended to the report instead. Anything said after the
// runner has accepted the finish is always appended.
let report = "";
let notes = [];
function commitTurn(afterFinish) {
  const t = turnText.trim();
  turnText = "";
  if (!t || t === report || notes.includes(t)) return;
  if (report && (afterFinish || (report.length >= 300 && t.length < report.length / 2))) notes.push(t);
  else {
    report = t;
    notes = [];
  }
}
const finalReport = () =>
  notes.length ? `${report}\n\n---\nLater note${notes.length > 1 ? "s" : ""} (after the report):\n\n${notes.join("\n\n")}` : report;
let tools = 0;
let lastTool = "";
let lastProgress = 0;
let stopped = false;

const brief = (v) => {
  try {
    const s = typeof v === "string" ? v : JSON.stringify(v);
    return s.length > 160 ? `${s.slice(0, 160)}…` : s;
  } catch {
    return "";
  }
};

function progress(force = false) {
  const now = Date.now();
  if (!force && now - lastProgress < 2000) return;
  lastProgress = now;
  write("progress.json", { req, attempt, host, tools, lastTool, lastText: lastText.slice(-2000), at: now });
}

function handle(line) {
  let e;
  try {
    e = JSON.parse(line);
  } catch {
    return;
  }
  if (e.type === "message_end" && e.message?.role === "assistant") {
    const content = Array.isArray(e.message.content) ? e.message.content : [];
    const said = content.filter((c) => c.type === "text").map((c) => c.text).join("\n").trim();
    if (said) {
      lastText = said;
      turnText = said;
      logLine(`> ${said.replace(/\n+/g, " ").slice(0, 600)}`);
    }
    errorText = e.message.stopReason === "error" ? e.message.errorMessage || "The model returned an error." : "";
    progress(true);
  }
  if (e.type === "tool_execution_start") {
    tools += 1;
    lastTool = `${e.toolName} ${brief(e.args)}`;
    logLine(`- ${lastTool}`);
    progress();
  }
}

for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  process.on(signal, () => {
    stopped = true;
    killLiveWork();
    try {
      child?.kill("SIGTERM");
    } catch {}
  });
}

// ---------------------------------------------------------------- live work
//
// A run may not finish while work it started is still going: a background
// process (`cmd &`, nohup, a Monitor), or a Zeron chat it created or sent a
// message to. Pi runs in RPC mode so it stays up between turns (a Monitor's
// completion wakes it); when it settles with live work left, the runner
// doesn't accept the finish: it tells the agent to wait, and when the work
// ends it asks for the report.

const threadId = env.HERON_THREAD_ID;
const SHELLS = /^(\S*\/)?(ba|z|fi|da|k)?sh(\s|$)/;
const tracked = new Map(); // pid -> command: processes seen under Pi's shells
const chatIds = new Map(); // Zeron chats this run created or messaged -> when
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
const CHAT_TOOLS = /create_chats?|send_messages?|wait_for_turn/;

function ps() {
  const out = spawnSync("ps", ["-axww", "-o", "pid=,ppid=,stat=,command="], { encoding: "utf8" }).stdout || "";
  const rows = new Map();
  for (const line of out.split("\n")) {
    const m = line.match(/^\s*(\d+)\s+(\d+)\s+(\S+)\s+(.*)$/);
    if (m) rows.set(Number(m[1]), { ppid: Number(m[2]), stat: m[3], cmd: m[4] });
  }
  return rows;
}

function kidsOf(rows) {
  const kids = new Map();
  for (const [pid, r] of rows) {
    if (!kids.has(r.ppid)) kids.set(r.ppid, []);
    kids.get(r.ppid).push(pid);
  }
  return kids;
}

function subtree(kids, root) {
  const out = new Set();
  const stack = [root];
  while (stack.length) {
    const p = stack.pop();
    for (const k of kids.get(p) || []) if (!out.has(k)) out.add(k), stack.push(k);
  }
  return out;
}

/** Pi's own process: the launcher may be a shell script that didn't exec. */
function piPid(rows, kids) {
  let p = child?.pid;
  while (p && rows.has(p) && SHELLS.test(rows.get(p).cmd) && (kids.get(p) || []).length === 1) p = kids.get(p)[0];
  return p;
}

/** Processes under Pi's shells (tool calls, Monitors), remembered so a job
 * that outlives its shell (`cmd &`) is still known after it is orphaned. */
function trackProcesses() {
  if (!child?.pid) return;
  const rows = ps();
  const kids = kidsOf(rows);
  for (const top of kids.get(piPid(rows, kids)) || []) {
    if (!SHELLS.test(rows.get(top).cmd)) continue; // an MCP server or the model's helper
    for (const p of [top, ...subtree(kids, top)]) tracked.set(p, rows.get(p).cmd);
  }
}

/** Processes marked as this thread's: its id in the environment (Pi sets
 * HERON_THREAD_ID; macOS hides it for system binaries) or a working folder
 * inside the thread's folder. */
function markedProcesses() {
  const found = new Set();
  let root = path.resolve(dir);
  try {
    root = fs.realpathSync(dir);
  } catch {}
  if (process.platform === "linux") {
    const uid = process.getuid();
    for (const name of fs.readdirSync("/proc")) {
      if (!/^\d+$/.test(name)) continue;
      try {
        if (fs.statSync(`/proc/${name}`).uid !== uid) continue;
        const cwd = fs.readlinkSync(`/proc/${name}/cwd`);
        if (cwd === root || cwd.startsWith(`${root}/`)) { found.add(Number(name)); continue; }
        if (fs.readFileSync(`/proc/${name}/environ`, "latin1").split("\0").includes(`HERON_THREAD_ID=${threadId}`)) found.add(Number(name));
      } catch {}
    }
    return found;
  }
  const withEnv = spawnSync("ps", ["-E", "-axww", "-o", "pid=,command="], { encoding: "utf8", maxBuffer: 64 << 20 }).stdout || "";
  const marker = ` HERON_THREAD_ID=${threadId}`;
  for (const line of withEnv.split("\n")) {
    const m = line.match(/^\s*(\d+)\s/);
    if (m && (line.includes(`${marker} `) || line.endsWith(marker))) found.add(Number(m[1]));
  }
  const cwds = spawnSync(fs.existsSync("/usr/sbin/lsof") ? "/usr/sbin/lsof" : "lsof", ["-a", "-d", "cwd", "-u", String(process.getuid()), "-Fpn"], { encoding: "utf8", maxBuffer: 64 << 20 }).stdout || "";
  let pid = 0;
  for (const line of cwds.split("\n")) {
    if (line[0] === "p") pid = Number(line.slice(1));
    else if (line[0] === "n") {
      const cwd = line.slice(1);
      if (cwd === root || cwd.startsWith(`${root}/`)) found.add(pid);
    }
  }
  return found;
}

/** Background processes this run started that are still running. */
function liveProcesses() {
  const rows = ps();
  const kids = kidsOf(rows);
  const skip = new Set([process.pid]);
  for (let p = process.ppid; p > 1 && rows.has(p) && !skip.has(p); p = rows.get(p).ppid) skip.add(p); // whoever started the runner
  for (const p of kids.get(process.pid) || []) skip.add(p); // Pi itself, and ps/lsof
  if (child?.pid) {
    const me = piPid(rows, kids);
    skip.add(me);
    for (const top of kids.get(me) || []) {
      if (!SHELLS.test(rows.get(top).cmd)) for (const p of [top, ...subtree(kids, top)]) skip.add(p);
    }
  }
  const live = new Set();
  for (const [p, cmd] of tracked) if (rows.get(p)?.cmd === cmd) live.add(p);
  for (const p of markedProcesses()) if (rows.has(p)) live.add(p);
  for (const p of [...live]) if (skip.has(p) || !rows.has(p) || rows.get(p).stat.startsWith("Z")) live.delete(p);
  // Name each job once: its topmost live process.
  return [...live].filter((p) => !live.has(rows.get(p).ppid)).map((p) => ({ kind: "process", pid: p, what: `process ${p}: ${rows.get(p).cmd.slice(0, 120)}` }));
}

function zeronExe() {
  for (const p of ["/Applications/Zeron.app/Contents/MacOS/zeron", path.join(HOME, ".local", "bin", "zeron")]) if (fs.existsSync(p)) return p;
  return "zeron";
}

/** The Zeron chats this run created or messaged that are still working. */
function liveChats() {
  if (!chatIds.size) return Promise.resolve([]);
  return new Promise((resolve) => {
    const p = spawn(zeronExe(), ["mcp"], { env, stdio: ["pipe", "pipe", "ignore"] });
    const ids = [...chatIds.keys()];
    const found = [];
    let buf = "";
    let left = ids.length;
    const done = () => {
      clearTimeout(timer);
      try { p.kill(); } catch {}
      resolve(found);
    };
    const timer = setTimeout(done, 20000);
    p.on("error", done);
    p.stdout.on("data", (c) => {
      buf += c.toString("utf8");
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        let msg;
        try { msg = JSON.parse(line); } catch { continue; }
        if (msg.id === 0) {
          p.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
          ids.forEach((chat, n) => p.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: n + 1, method: "tools/call", params: { name: "get_chat", arguments: { chat } } })}\n`));
          continue;
        }
        if (typeof msg.id !== "number") continue;
        try {
          const res = msg.result;
          if (res && !res.isError) {
            const c = JSON.parse(res.content.map((x) => x.text || "").join(""));
            const heron = c.title === "Heron" && /^~\/heron\/?$/.test(c.cwd || "");
            const starting = Date.now() - (chatIds.get(ids[msg.id - 1]) || 0) < 20000 && c.sessionStatus === "idle"; // just messaged
            if (!heron && (c.sessionStatus === "working" || starting)) found.push({ kind: "chat", id: c.id, what: `Zeron chat ${c.id} "${c.title || ""}" (still working)` });
          } else if (res?.isError) chatIds.delete(ids[msg.id - 1]); // not a chat id
        } catch {}
        if (--left <= 0) done();
      }
    });
    p.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 0, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "heron-runner", version: "1" } } })}\n`);
  });
}

async function liveWork() {
  return [...liveProcesses(), ...(await liveChats())];
}

const chatCalls = new Set(); // tool calls that create or message a chat
function noteChats(e) {
  if (e.type !== "tool_execution_start" && e.type !== "tool_execution_end") return;
  let s;
  try { s = JSON.stringify(e.type === "tool_execution_start" ? e.args : e.result); } catch { return; }
  if (!s) return;
  if (e.type === "tool_execution_start" ? !CHAT_TOOLS.test(s) && !CHAT_TOOLS.test(e.toolName || "") : !chatCalls.has(e.toolCallId) && !CHAT_TOOLS.test(s)) return;
  if (e.type === "tool_execution_start") chatCalls.add(e.toolCallId);
  else chatCalls.delete(e.toolCallId);
  for (const id of s.match(UUID) || []) chatIds.set(id.toLowerCase(), Date.now());
}

const WAIT_NUDGES = 3; // tell the agent to wait at most this often; then just wait
const MAX_WAIT_MS = 4 * 3600 * 1000; // give up waiting on background work after this
const GRACE_MS = 20000; // after the work ends, let a Monitor's notice start a turn first

const waitNote = (live) => live.map((w) => `- ${w.what}`).join("\n");
const nudgeText = (live) =>
  `[Heron runner] You ended your turn, but work you started is still running:\n${waitNote(live)}\n\n` +
  "The task can't finish while it runs: nobody would get its result. Wait for it now (block on it: a Monitor's notice will wake you, or poll it in a foreground loop, or wait_for_turn on a chat), then give your final report. " +
  "If you don't need it any more, stop it (kill the process, monitor_stop, interrupt the chat) and then report.";
const finishedText =
  "[Heron runner] The background work you were waiting on has finished. Check its result, then give your final report.";

function setWaiting(live) {
  const waitingOn = live ? live.map((w) => w.what) : undefined;
  const st = read("status.json", {});
  if (JSON.stringify(st.waitingOn) === JSON.stringify(waitingOn)) return;
  write("status.json", { ...st, waitingOn });
}

/** One Pi run on `model` (RPC mode): resolves with its exit code once the
 * agent has settled with no live work left. */
function runOnce(model, text) {
  return new Promise((resolve) => {
    const args = ["--mode", "rpc", "--session-dir", file("session"), "--no-context-files", "--append-system-prompt", file("brief.md")];
    if (model) args.push("--model", model);
    args.push("--name", `Heron task: ${spec.title}`);
    errorText = "";
    lastText = ""; // a run that failed on a limit said only that; it is not the report
    turnText = "";
    report = "";
    notes = [];
    let pending = "";
    let busy = true; // an agent run is (about to be) under way
    let settles = 0;
    let nudged = 0;
    let nudgedAt = -1; // the settle we last nudged after
    let waitingSince = 0;
    let workEndedAt = 0;
    let liveAtSettle = -1; // the settle count when live work was last seen
    let finishing = false;
    let checking = false;
    child = spawn(pi, args, { cwd: file("work"), env, stdio: ["pipe", "pipe", "pipe"] });
    const send = (cmd) => {
      try {
        child.stdin.write(`${JSON.stringify(cmd)}\n`);
      } catch {}
    };
    const prompt = (message) => {
      busy = true;
      send({ type: "prompt", message });
    };
    const finish = () => {
      if (finishing) return;
      finishing = true;
      clearInterval(timer);
      setWaiting(null);
      try {
        child.stdin.end();
      } catch {}
      setTimeout(() => { try { child.kill("SIGTERM"); } catch {} }, 30000).unref();
    };
    const check = async () => {
      if (busy || finishing || checking) return;
      checking = true;
      try {
        if (stopped || errorText) return finish(); // a model error: the caller may retry on another model
        const live = await liveWork();
        if (busy || finishing) return;
        if (live.length) {
          workEndedAt = 0;
          liveAtSettle = settles;
          if (!waitingSince) {
            waitingSince = Date.now();
            logLine(`(not finished: still running\n${waitNote(live)})`);
          }
          setWaiting(live);
          if (Date.now() - waitingSince > MAX_WAIT_MS) {
            logLine(`(gave up waiting after ${MAX_WAIT_MS / 3600000}h)`);
            return finish();
          }
          if (nudgedAt !== settles && nudged < WAIT_NUDGES) {
            nudged += 1;
            nudgedAt = settles;
            prompt(nudgeText(live));
          }
          return;
        }
        if (!waitingSince) return finish();
        // The work ended and a turn has run since (its notice woke the agent,
        // or it polled until the end): that turn gave the report.
        if (settles > liveAtSettle) {
          logLine("(background work finished; the agent has reported)");
          return finish();
        }
        // The work it waited on is over and no turn has picked that up.
        if (!workEndedAt) workEndedAt = Date.now();
        if (Date.now() - workEndedAt < GRACE_MS) return;
        logLine("(background work finished; asking for the report)");
        waitingSince = 0;
        workEndedAt = 0;
        setWaiting(null);
        prompt(finishedText);
      } finally {
        checking = false;
      }
    };
    const timer = setInterval(() => {
      trackProcesses();
      check();
    }, 2000);
    child.stdin.on("error", () => {});
    child.stderr.on("data", (c) => errors.write(c));
    child.stdout.on("data", (chunk) => {
      pending += chunk.toString("utf8");
      let i;
      while ((i = pending.indexOf("\n")) >= 0) {
        const line = pending.slice(0, i).replace(/\r$/, "");
        pending = pending.slice(i + 1);
        if (!line) continue;
        let e;
        try {
          e = JSON.parse(line);
        } catch {
          continue;
        }
        if (e.type === "extension_ui_request") {
          // Nobody can answer a dialog: decline it.
          if (["select", "confirm", "input", "editor"].includes(e.method)) send({ type: "extension_ui_response", id: e.id, cancelled: true });
          continue;
        }
        if (e.type === "message_update") continue; // streaming deltas; message_end has the text
        events.write(`${line}\n`);
        if (e.type === "response") {
          if (e.command === "prompt" && (!e.success || e.data?.disposition === "handled")) {
            if (!e.success) errorText = e.error || "Pi rejected the prompt.";
            busy = false;
            check();
          }
          continue;
        }
        if (e.type === "agent_start") busy = true;
        if (e.type === "tool_execution_start") trackProcesses();
        noteChats(e);
        handle(line);
        if (e.type === "agent_settled") {
          busy = false;
          settles += 1;
          commitTurn(finishing);
          check();
        }
      }
    });
    child.on("error", (err) => {
      errorText = `Couldn't start Pi (${pi}): ${err.message}`;
    });
    child.on("close", (code) => {
      clearInterval(timer);
      if (pending) handle(pending);
      commitTurn(finishing);
      resolve(finishing && code === null ? 0 : code);
    });
    prompt(text);
  });
}

/** On stop: end the background work this thread started. */
function killLiveWork() {
  try {
    for (const w of liveProcesses()) {
      try { process.kill(-w.pid, "SIGTERM"); } catch {}
      try { process.kill(w.pid, "SIGTERM"); } catch {}
    }
  } catch {}
}

const stderrTail = () => {
  try {
    return fs.readFileSync(file("stderr.log"), "utf8").trim().split("\n").slice(-5).join("\n");
  } catch {
    return "";
  }
};

const models = heronModels();
let code = 1;
let error = null;
for (let i = 0; i < models.length; i++) {
  const text = i === 0 ? prompt : `${prompt}\n\n---\nAn earlier run of this task on another model stopped (${error || "model error"}). Its log so far:\n${clip(readText("log.md"), 6000)}\n\nContinue from where it stopped.`;
  code = await runOnce(models[i], text);
  error = errorText || (code !== 0 ? `Pi exited with code ${code}${stderrTail() ? `: ${stderrTail()}` : ""}` : null);
  if (stopped || !error) break;
  if (i + 1 < models.length && MODEL_TROUBLE.test(error)) {
    logLine(`(${models[i]} failed: ${error.slice(0, 200)}; switching to ${models[i + 1]})`);
    continue;
  }
  break;
}

progress(true);
// The lease moved on while this ran (this computer was presumed down):
// the new owner writes the outcome, not us.
const lease = read("lease.json", {});
if (lease.req === req && lease.attempt !== attempt) {
  logLine(`(attempt ${attempt} on ${host} ended after the task moved to ${lease.ownerName || "another computer"})`);
  process.exit(0);
}
const state = stopped ? "stopped" : error ? "failed" : "done";
const result = finalReport() || lastText || "";
fs.writeFileSync(file(`result-${req}.md`), result);
fs.writeFileSync(file("result.md"), result);
logLine(`(${state}${error ? `: ${error.slice(0, 300)}` : ""})`);
write("status.json", { ...read("status.json", {}), state, req, attempt, host, exitCode: code, error: stopped ? null : error, endedAt: Date.now(), pid: null });
events.end();
errors.end();
