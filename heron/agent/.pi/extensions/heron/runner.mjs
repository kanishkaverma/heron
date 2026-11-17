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
import { spawn } from "node:child_process";
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

// ---------------------------------------------------------------- the model

function heronModel() {
  try {
    const s = JSON.parse(fs.readFileSync(path.join(HOME, "heron", ".pi", "settings.json"), "utf8"));
    if (s.defaultProvider && s.defaultModel) return `${s.defaultProvider}/${s.defaultModel}`;
  } catch {}
  return null;
}

const args = ["-p", "--mode", "json", "--session-dir", file("session"), "--no-context-files", "--append-system-prompt", file("brief.md")];
const model = spec.model || heronModel();
if (model) args.push("--model", model);
args.push("--name", `Heron task: ${spec.title}`, "--", prompt);

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
  // The lease moved on while this ran (this computer was presumed down):
  // the new owner writes the outcome, not us.
  const lease = read("lease.json", {});
  if (lease.req === req && lease.attempt !== attempt) {
    logLine(`(attempt ${attempt} on ${host} ended after the task moved to ${lease.ownerName || "another computer"})`);
    process.exit(0);
  }
  const state = stopped ? "stopped" : code === 0 && !errorText ? "done" : "failed";
  let error = errorText || null;
  if (state === "failed" && !error) {
    const tail = fs.existsSync(file("stderr.log")) ? fs.readFileSync(file("stderr.log"), "utf8").trim().split("\n").slice(-5).join("\n") : "";
    error = `Pi exited with code ${code}${tail ? `: ${tail}` : ""}`;
  }
  fs.writeFileSync(file(`result-${req}.md`), lastText || "");
  fs.writeFileSync(file("result.md"), lastText || "");
  logLine(`(${state}${error ? `: ${error.slice(0, 300)}` : ""})`);
  const now = read("status.json", {});
  write("status.json", { ...now, state, req, attempt, host, exitCode: code, error, endedAt: Date.now(), pid: null });
  events.end();
  errors.end();
});
