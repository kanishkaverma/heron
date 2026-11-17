# Working on Heron

Heron is a one-chat iPhone assistant on a fork of Zeron. What it is and where
each piece lives: [`HERON.md`](HERON.md). This file is how to change it fast.

## The loop for each layer

Use the tightest loop that can go red on your change. The phone and GitHub
Actions are final checks, never the loop.

| You changed | Loop | Time |
| --- | --- | --- |
| Leasing, failover, takeover, model fallback (`heron/keeper/heron-keeper.py`) | `python3 heron/keeper/test_keeper.py`: add the scenario there first | under 1 s |
| What Heron says or does (`heron/agent/AGENTS.md`, the extension) | `heronctl deploy`, then `heronctl chat send "…" --wait 120` (the dev chat) | about 10 s |
| Background tasks (`runner.mjs`, scheduling) | `heronctl task start "Title" "brief"`, `heronctl wait`, `heronctl task show ID` | about 30 s |
| The app's Swift | `scripts/heron/sim.sh` on a Mac (simulator, real account) | about 10 s after the first build |
| Anything, before saying it works | the `verify-heron` skill | minutes |

`heronctl` is `scripts/heron/heronctl`; `heronctl -h` lists every command.

## Rules that cost us before

- **Reach the computers through `heronctl`**, not hand-written `ssh` with
  quoted JSON: the MacBook's login shell is fish, PATH differs per computer,
  and air is launchd while zeph is systemd. Missing a command? Add it there.
- **Wait on an event, not a sleep**: `heronctl wait 'REGEX'` returns when a
  keeper log line matches, on any computer.
- **Test on the dev chat and dev tasks.** `heronctl chat send` talks to a chat
  titled "Heron dev", which the app and the keeper ignore; `--live` is the
  person's real chat and they are probably using it. `heronctl task start`
  makes tasks no chat hears about.
- **A test that runs `memo` or `memo-fix` sets `MEMORY_DIR` to a temp folder
  on the same line.** Unset, it writes the person's real memory.
- **`heronctl diff` before `heronctl deploy`.** A difference you did not make
  means another checkout is newer than yours: deploying would undo it.
- **After `heronctl kill` or `test-mode on`, finish with `heronctl revive` and
  `heronctl test-mode off`**, then `heronctl status` (exit 0 = all up).
- **Commit and push what you deploy.** `AGENTS.md` files are in a global
  gitignore here: `git add -f` them.

## Per-computer facts no config shows

- Names: air is `nishka-server` in Zeron, and the first in the keeper's
  priority order (air, zeph, MacBook Pro), so it leads and hosts the chat.
- Each computer has its own working models (`~/.heron/models.json`); logins
  are not shared. A 401 or a limit on one says nothing about the others.
- Keeper logs carry each computer's local time.
- A running chat keeps the extension it started with; a deploy reaches it
  when its agent next starts.
