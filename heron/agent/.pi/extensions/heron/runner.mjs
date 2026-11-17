#!/usr/bin/env node
// Runs one Heron background thread to the end, detached from the chat that
// started it: `node runner.mjs <thread dir>`. It runs `pi -p --mode json` on
// run.json's prompt (resuming the thread's own session for follow-ups),
// keeps progress.json current, and on exit writes result.md and the final
// status.json for the Heron extension to report.
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const dir = process.argv[2];
if (!dir) {
  console.error("usage: runner.mjs <thread dir>");
  process.exit(2);
}
const file = (f) => path.join(dir, f);
const read = (f, fallback) => {
  try {
    return JSON.parse(fs.readFileSync(file(f), "utf8"));
  } catch {
    return fallback;
  }
};
const write = (f, value) => {
  const tmp = `${file(f)}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2));
  fs.renameSync(tmp, file(f));
};

const spec = read("spec.json", {});
const run = read("run.json", {});
const status = read("status.json", {});
write("status.json", { ...status, state: "running", pid: process.pid, startedAt: status.startedAt || Date.now() });

const args = ["-p", "--mode", "json", "--session-dir", file("session"), "--no-context-files", "--append-system-prompt", file("brief.md")];
if (spec.model) args.push("--model", spec.model);
if (run.resume && status.sessionId) args.push("--session", status.sessionId);
else if (run.resume) args.push("--continue");
else args.push("--name", `Heron thread: ${spec.title}`);
args.push("--", run.prompt || spec.task || "");

// The chat's Zeron identity stays with the chat.
const env = { ...process.env, HERON_THREAD_ID: spec.id || path.basename(dir) };
for (const key of Object.keys(env)) if (key.startsWith("ZERON_")) delete env[key];

const pi = process.env.HERON_PI || "pi";
fs.mkdirSync(file("work"), { recursive: true });
const child = spawn(pi, args, { cwd: file("work"), env, stdio: ["ignore", "pipe", "pipe"] });

const events = fs.createWriteStream(file("events.jsonl"), { flags: "a" });
const errors = fs.createWriteStream(file("stderr.log"), { flags: "a" });
child.stderr.pipe(errors);

let pending = "";
let lastText = "";
let errorText = "";
let sessionId = status.sessionId || null;
let tools = 0;
let lastTool = "";
let lastProgress = 0;
let stopped = false;

const brief = (args) => {
  try {
    const s = typeof args === "string" ? args : JSON.stringify(args);
    return s.length > 160 ? `${s.slice(0, 160)}…` : s;
  } catch {
    return "";
  }
};

function progress(force = false) {
  const now = Date.now();
  if (!force && now - lastProgress < 2000) return;
  lastProgress = now;
  write("progress.json", { tools, lastTool, lastText: lastText.slice(-2000), at: now });
}

function handle(line) {
  let e;
  try {
    e = JSON.parse(line);
  } catch {
    return;
  }
  if (e.type === "session" && e.id) sessionId = e.id;
  if (e.type === "message_end" && e.message?.role === "assistant") {
    const content = Array.isArray(e.message.content) ? e.message.content : [];
    const said = content.filter((c) => c.type === "text").map((c) => c.text).join("\n").trim();
    if (said) lastText = said;
    errorText = e.message.stopReason === "error" ? e.message.errorMessage || "The model returned an error." : "";
    progress(true);
  }
  if (e.type === "tool_execution_start") {
    tools += 1;
    lastTool = `${e.toolName} ${brief(e.args)}`;
    progress();
  }
}

child.stdout.on("data", (chunk) => {
  events.write(chunk);
  pending += chunk.toString("utf8");
  let i;
  while ((i = pending.indexOf("\n")) >= 0) {
    const line = pending.slice(0, i).replace(/\r$/, "");
    pending = pending.slice(i + 1);
    if (line) handle(line);
  }
});

for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  process.on(signal, () => {
    stopped = true;
    try {
      child.kill("SIGTERM");
    } catch {}
  });
}

child.on("error", (err) => {
  errorText = `Couldn't start Pi (${pi}): ${err.message}`;
});

child.on("close", (code) => {
  if (pending) handle(pending);
  progress(true);
  fs.writeFileSync(file("result.md"), lastText || "");
  const now = read("status.json", {});
  const state = stopped ? "stopped" : code === 0 && !errorText ? "done" : "failed";
  let error = errorText || null;
  if (state === "failed" && !error) {
    const tail = fs.existsSync(file("stderr.log")) ? fs.readFileSync(file("stderr.log"), "utf8").trim().split("\n").slice(-5).join("\n") : "";
    error = `Pi exited with code ${code}${tail ? `: ${tail}` : ""}`;
  }
  try {
    fs.rmSync(file("notified.lock"), { recursive: true, force: true });
  } catch {}
  write("status.json", { ...now, state, exitCode: code, error, sessionId, endedAt: Date.now(), pid: null, notified: false });
  events.end();
  errors.end();
});
