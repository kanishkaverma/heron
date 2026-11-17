# Background tasks

A task is a folder in `~/.heron/threads` (synced everywhere). The leader
keeper leases it to the least busy online computer; that computer's keeper
runs it with `runner.mjs`.

## Sub-features

- Leased within a tick or two, started on the leased computer
- Runs in its `work/` folder, writes `log.md` and a result
- Stop, and follow-up requests

## How to get to it (user POV)

The person asks for something long; Heron calls `thread_start` and later
tells them the report.

## Driving it with heronctl

```
T=$($H task start "Ping" 'Use only bash. Run hostname and report just the output.')
$H wait "task $T: started" --timeout 90
$H tasks                      # state running, later done
$H task show $T               # state done, the log, the result
$H task rm $T
```

Proof: the `wait` prints the `started request 1 attempt 1` line of the
computer that got it, and `task show` prints `state done` with the hostname
of that same computer in the result.

Through the agent instead (proves `thread_start` and the report coming back
into a chat): `$H chat send 'Developer test… Start a background thread titled
"Ping" with the brief: "Use only bash. Run hostname and report it." Then end
your turn.' --wait 120`, then `$H chat read` until the report appears.

## Gotchas

- `heronctl task start` tasks belong to no chat, so nobody is told the
  result: read it with `task show`.
- `tasks` shows `waiting` until the owner's `status.json` has synced.
- Only `dev-` tasks can be stopped or removed with heronctl.
