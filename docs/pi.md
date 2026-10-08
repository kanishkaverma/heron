# Pi native RPC

Zeron launches `pi --mode rpc` directly. Pi 0.85.1 or newer is required; 0.85.1
is covered by a real-process test with a local mock provider. Protocol details
and the acceptance barrier are in [PROTOCOL.md](../crates/harness/src/pi/PROTOCOL.md).

Install and authenticate Pi through its CLI. `PI_EXECUTABLE` selects an explicit
Pi executable; otherwise Zeron uses its usual PATH, login-shell and install-dir
discovery. `PI_CODING_AGENT_DIR` continues to control Pi settings and credentials.
`PI_ACP_EXECUTABLE` and `PI_ACP_PI_COMMAND` are no longer used. Zeron neither
installs nor launches `pi-acp`, and existing adapter files need not be deleted.

Sessions retain their native UUID and JSONL history. Zeron records the UUID to
absolute file mapping in `PI_CODING_AGENT_DIR/zeron-sessions` (normally
`~/.pi/agent/zeron-sessions`). For older chats it also reads
`~/.pi/pi-acp/session-map.json`, then searches native session directories,
including configured `sessionDir` locations. It validates the session header
before reopening the exact file. A session that cannot be found starts a new
conversation with a visible notice, as other harnesses do. A session that Pi has not yet
written can retain its UUID through `--session-id` only after ordered native
queries prove it has no conversation or extension entries. The proof is revoked
before another input is sent. Unsaved custom extension state cannot be recreated:
Pi only materializes it after its first assistant response, so if that file
never existed the chat continues in a new session with the same notice. Session changes made by extensions
refresh the UUID and file mapping before turn completion.

Models and supported thinking levels come from native RPC discovery. The
Thinking option can disable reasoning. Unsupported saved effort levels are
clamped to a supported lower level. Discovery uses a temporary no-session
process; model switches do not persist global defaults. Commands and skills
are discovered in the workspace. Pi controls project-extension trust.

Steering queues at a model step boundary and starts immediately when idle.
When no steering mode is configured, Zeron selects Pi's `all` mode, so messages
queued before the next model call enter that call together. Pi persists this
mode in its global settings. A mode already set in Pi's global or project
settings (including one chosen with `/steering`) is never overwritten.
Each input is sent as soon as the preceding preflight and ordered state query
finish, without waiting for earlier queued inputs to be consumed or adding a
batching delay. Inputs arriving after a model call starts belong to a later step.
Zeron confirms each original message only when Pi consumes it. Extension commands
and inputs handled without a model run retain serialized delivery.
Interrupt clears queues, aborts
the run and terminates the owned process tree after a grace period. A completed
model iteration (`agent_end`) alone does not close the turn: retries,
compaction and handled extension commands follow the native lifecycle.

Extension `select`, `confirm`, `input` and `editor` dialogs use Zeron questions.
Editor content preserves whitespace and prefill; desktop supports Shift+Enter
and iOS uses a multiline editor. Timeouts and interruption remove pending
questions. Notifications appear in the transcript. Terminal-only extension UI
such as widgets and custom TUI components is not rendered.

Zeron's existing delegation uses a temporary MCP bridge loaded with
`--extension`. Tools and chat identity retain the existing engine contract;
no Pi subagent extension is installed. Image attachments are sent as native
image blocks; image-only tool results do not yet render inline in Zeron.

`/tree` in a Pi chat opens a palette over the conversation's branches. Zeron
reads the tree from Pi's session file and shows what Pi's own `/tree` shows by
default, minus tool results. Pi's RPC has no navigate command and only an
extension command can call `ctx.navigateTree`, so every run also loads a small
`--extension` that registers `/zeron-tree-jump <entryId> [summarize]`. The
command never reaches the slash menu. Jumping to a user message rewinds to just
before it and returns its text to the composer. Pi keeps the leaf in memory but
resumes a session at the last entry of its file, so a jump appends one
`zeron-tree-jump` entry at the new leaf, which is what lets it survive a
process restart. A summarising jump asks Pi for a summary only when the branch
it leaves holds conversation, since a new process and every jump leave
bookkeeping entries at the leaf. The palette is unavailable while a run is
live, with staged attachments, in a side chat, and in other agents' chats. A
parked Pi's output in the first second after a turn is dropped as that turn's
tail, so a jump that quick has no confirmation line, though it still happens.

Validation:

```sh
cargo test -p zeron-harness --features native-fixture
cargo test -p zeron-harness --test pi_session_tree
cargo test -p zeron-engine --test session_tree
cargo test -p zeron-engine --lib --test pi_resume --test acp_lifecycle --test message_queue --test e2e
cargo test -p zeron-harness --test pi_live -- --ignored --nocapture
cargo test -p zeron-ui --lib -- --test-threads=1 --ignored session_tree
cargo check -p zeron-ui --tests
cargo build -p zeron-mobile --features bindgen
```

The ignored Pi test requires an installed CLI and uses isolated settings and a
local provider; it makes no model API requests. Native iOS UI compilation still
requires Xcode on macOS.
