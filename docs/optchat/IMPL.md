# OptChat in Zeron: implementation contract

`SPEC.md` (same folder) is the behavior. This file fixes where it lives in
Zeron, the data shapes, and how it is proven. Delegates read both and do not
edit either.

## Placement

- `crates/optchat` (package `zeron-optchat`) owns everything OptChat: the log,
  the tree, the compactor, the view, the turn loop, the tools, the two model
  clients, the login store reader. It depends on `zeron-proto` and
  `zeron-harness` only. No GPUI, no engine.
- Zeron sees it as `HarnessId::OptChat`, an in-process `Harness` (no CLI),
  registered `Ready` in `crates/engine/src/registry.rs::default_registry`.
- The engine binds the memory home once, in `EngineCore::assemble_with_profile`:
  `zeron_optchat::bind(Binding { home: <device_root>/optchat, credentials:
  Arc::new(FileCredentials::new(home.join("auth.json"))) })`.
- Logins: OptChat keeps its OWN OAuth logins, separate from Claude Code's
  keychain item and `~/.codex` (refreshing those would revoke the CLIs' own
  refresh tokens). Store: `<home>/auth.json`, pi's JSON shape (see
  `FileCredentials` doc). The engine's keyed-account machinery
  (`agent_accounts/stores.rs::keyed_accounts`, `keyed_file`,
  `write_keyed_entry`) lists and signs out of it like Pi's store: keys
  `anthropic` and `openai-codex`, lock `StoreLock::File(<home>/auth.json.lock)`.
- The sign-in flows themselves live in `zeron_optchat::auth`, so the headless
  e2e driver and the app run the same code:
  `auth::start_login(provider, store_path) -> Result<LoginStart>` where
  `LoginStart { url, mode: Browser|PasteCode, code: Option<oneshot::Sender<String>>,
  done: JoinHandle<Result<String /*account label*/, String>> }`. The flow
  binds the loopback listener, serves the callback page, exchanges the code,
  and writes the entry under the lock. The engine's `start_login_with` branch
  for `HarnessId::OptChat` adapts it to its `LoginFlow::Task` + `complete_login`.
  - ChatGPT: PKCE on `https://auth.openai.com/oauth/authorize`, loopback
    `http://localhost:1455/auth/callback` (the client id only accepts 1455),
    scope `openid profile email offline_access`,
    `id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=zeron`.
  - Claude: PKCE on `https://claude.ai/oauth/authorize?code=true`, Claude Code
    client id, loopback `http://localhost:53692/callback` (state = verifier),
    scope `org:create_api_key user:profile user:inference
    user:sessions:claude_code user:mcp_servers user:file_upload`; paste-code
    fallback (`code#state`, redirect `https://platform.claude.com/oauth/code/callback`)
    when the port can't be bound. Exchange at
    `https://platform.claude.com/v1/oauth/token` (JSON, includes `state`).

## Data shapes (crates/optchat)

```rust
enum Kind { User, Talk, Tool, Echo, Note }                  // serde lowercase
struct Message { i: u64, kind: Kind, text: String, size: u64, date: String }
struct Node    { l: u32, i: u64, text: String, size: u64 }
struct Part    { l: u32, i: u64 }                           // one view entry
struct Memory  {                                            // PURE: no IO, no async
    root: Vec<Message>,
    tree: HashMap<(u32, u64), Node>,
    view: Vec<Part>,
}
```

`Memory` is the only place the tree/view math lives: `append(message)`,
`insert(node)`, `fit(budget)`, `first_unbuilt()`, `ready(l, i)`,
`render_view()`, `render_context(upto)`, `zoom(id, n)`. Everything else
(store, compactor, turn loop) calls it. The fold is a pure function of the
log and the set of built nodes, so it can be replayed with no network.

Modules: `store` (jsonl append + fsync, torn lines, Unix-socket lock),
`memory`, `compactor` (pump, prompts verbatim from SPEC §4.4, size
enforcement §4.3), `llm::anthropic`, `llm::openai` (streaming SSE, native
transcripts kept verbatim), `turn` (SPEC §7: settle, fresh call, tools,
steering, CAP), `auth` (`FileCredentials`: read, refresh under flock, write
back), `harness` (the `Harness` impl mapping to `AgentEvent`).

## Wire facts (from the installed pi-ai 2026 dist; re-verify against the live API)

Anthropic OAuth (`POST https://api.anthropic.com/v1/messages`, stream):
`authorization: Bearer <access>`, `anthropic-version: 2023-06-01`,
`anthropic-beta: claude-code-20250219,oauth-2025-04-20`,
`user-agent: claude-cli/2.1.280`, `x-app: cli`. The FIRST system block must be
exactly `You are Claude Code, Anthropic's official CLI for Claude.`; OptChat's
MASTER+VIEW_DOC follows as a second block. Thinking on current models:
`thinking: {type: "adaptive", display: "summarized"}` plus
`output_config: {effort}`. Refresh: `POST https://platform.claude.com/v1/oauth/token`
JSON `{grant_type: refresh_token, client_id, refresh_token}`.
Client id `9d1c250a-e61b-44d9-88ed-5944d1962f5e`.

ChatGPT OAuth (`POST https://chatgpt.com/backend-api/codex/responses`, SSE):
`Authorization: Bearer`, `chatgpt-account-id`, `originator`,
`OpenAI-Beta: responses=experimental`, `accept: text/event-stream`,
`session-id`. Body: `store:false, stream:true, instructions, input,
include:["reasoning.encrypted_content"], prompt_cache_key, tool_choice:"auto",
parallel_tool_calls:true, tools, reasoning:{effort, summary:"auto"}`. Try the
SPEC §8 extras (`prompt_cache_breakpoint`, `reasoning.context:"all_turns"`);
keep each only if the live endpoint accepts it, and record the result.
Result (2026-10-05, probed live): `reasoning.context:"all_turns"` is accepted.
`prompt_cache_breakpoint` exists (`{"mode":"explicit"}` is its only shape) but
answers 400 "not supported on this model" for both gpt-6-luna and gpt-6-sol,
so it is not sent. The implicit cache hits when a request repeats a whole
earlier prompt (each step of a call: verified), and misses when a request
diverges from it midway, even at a content-part boundary. So on ChatGPT the
view is cached within a call, not across turns.
Refresh: `POST https://auth.openai.com/oauth/token` form
`{grant_type: refresh_token, refresh_token, client_id: app_EMoamEEZ73f0CkXaXp7hrann}`.
Account id: JWT claim `https://api.openai.com/auth`.`chatgpt_account_id`.

Models offered (`Harness::models`): `claude-opus-5-5`, `claude-sonnet-5-5`,
`gpt-6-sol`, `gpt-6-luna`. Provider = model prefix (`claude-` / `gpt-`).
Compactor: `claude-sonnet-5-5` at medium when Claude is signed in, else
`gpt-6-luna` at medium.

## Mapping onto Zeron's harness contract

- `run(request)`: `request.prompt` is the user's new message. Ignore
  `resume`; return a constant session id `optchat`. Settle (SPEC §6), render the
  view, THEN log the prompt as `user`, then one fresh model call.
- `controls.steering`: mid-run user messages. Deliver at the next tool
  boundary, log as `user`. Any still queued when the call ends start the next
  fresh call in the same run (SPEC §7 `while queue not empty`).
- `controls.interrupt`: cancels the settle wait or the call. End with
  `Done { status: Interrupted }`. Untaken messages stay logged.
- Emit `ReasoningDelta` for thoughts (never logged), `TextDelta` for replies,
  `ToolCall`/`ToolResult` for tools, `Usage`, then `Done`.
- Tools: `zoom`, `date` (SPEC §7.1, descriptions verbatim) plus `bash`
  (`ToolCall::Exec`, runs in `request.cwd`), `read` (`ToolCall::ReadFile`),
  `write` (`ToolCall::WriteFile`), `edit` (`ToolCall::EditFile`). Results capped
  at CAP = 30,000 chars, head and tail kept.
- `commands()`: `/memory` writes the browsing page of SPEC §10 to
  `<home>/memory.html` and answers with its path.

## Ways it can fail (write the checks for these before the code)

1. View exceeds `VIEW` after `fit` while a buildable parent exists.
2. View splits a merged part, or a part of view_t is neither in view_t+1 nor
   covered by one of its parts.
3. View is recomputed rather than folded: consecutive views share little
   prefix (SPEC §5.3; the replay should share well past half).
4. A compactor call sees a placeholder line, or a node starts before its
   context is summarized (§4.1 rule 3).
5. Ids leak into a compactor input (§4.2).
6. A level-0 node over `NODE` is kept when a shorter try existed (§4.3).
7. A torn last line kills loading, or the next append lands on the torn line.
8. Two processes write the same chat (lock not held or not detected).
9. Unfsynced writes (a write returns before `sync_data`).
10. Load does not reproduce the live view (fold from message 0 must equal the
    view the previous process had).
11. A turn starts while a view line is unsummarized.
12. A thought is logged.
13. Mid-run steering is dropped, or reaches the model without being logged.
14. Cache misses across steps of one call (each step must read what the
    previous sent; check `cache_read` in usage), or across turns (the view
    breakpoints must hit).
15. Token expiry mid-session: refresh must happen once under the lock and be
    written back; a 401 retries once after refresh.
16. Zoom with bad arguments panics instead of answering `No line id+n.`
17. Interrupt during settle or during a tool leaves the run hanging.
18. Two runs at once (two Zeron chats on OptChat) interleave their steps in
    the one log. A run waits for the running one, cancellably.

## Verification contract

- `cargo run -p zeron-optchat --bin optchat-e2e -- fold-sim` replays thousands
  of synthetic messages through `Memory` with instantly built nodes of random
  realistic sizes and asserts failures 1-3 and 10, printing view size,
  lines, and median shared prefix between consecutive renders.
- `cargo run -p zeron-optchat --bin optchat-e2e -- login <anthropic|openai> --home <dir>`
  prints the authorize URL, waits for the loopback (or a pasted code on
  stdin), and writes `<dir>/auth.json`. The parent completes it in a browser.
- `cargo run -p zeron-optchat --bin optchat-e2e -- live --home <tmp>` drives
  the harness exactly as the engine does (RunRequest + RunControls) against
  the real APIs with the logins in `<home>/auth.json`: several turns, one
  forcing a zoom down to n = 1, one steered mid-run, one interrupted, then a
  restart that reloads and compares the view. Writes a JSON report (usage per
  step incl. cache reads, assertions, files) to `<home>/e2e-report.json`.
- The app: build `scripts/run-macos-dev.sh` (isolated `Zeron Dev.app`), pick
  OptChat, sign in to Claude and ChatGPT from Settings, chat, screenshot.
