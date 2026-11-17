# The agent answers

Heron is a Pi agent in `~/heron` on one computer, with `heron/agent/AGENTS.md`
as its instructions and the Heron extension's tools.

## Sub-features

- Replies on this computer's working model
- Follows its instructions (memory at startup, short phone-sized replies)
- Has the thread tools (`thread_start`, `thread_list`, …)

## How to get to it (user POV)

The person types in the iPhone app's only chat.

## Driving it with heronctl

```
$H chat send "Developer test, not the person. Do not note any memory. Run hostname with bash and reply with just: pong <hostname>" --wait 150 --on zeph
```

Proof: exit 0 and an `assistant: pong <that computer>` line. Repeat `--on air`
and `--on mbp` to cover each computer's model and tools. `$H chat read` shows
the last messages again. To test new instructions or a new tool, ask for the
behaviour in the text and read the reply.

## Gotchas

- Start test messages with "Developer test, not the person. Do not note any
  memory." The dev chat shares Heron's real memory store.
- An existing dev chat keeps the extension it started with. After a deploy,
  `$H chat cleanup` and send again to get a fresh agent.
- `ERRORS:` under a reply and exit 1 mean the turn failed; a 429 or 401 is
  that computer's model, so try another computer.
