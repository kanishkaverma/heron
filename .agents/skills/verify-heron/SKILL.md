---
name: verify-heron
description: Verify Heron (the one-chat iPhone assistant and the keeper, tasks and failover behind it) against the real computers with scripted gates. Use after changing the keeper, the Heron Pi extension, Heron's AGENTS.md, a deploy, or the app, and before telling the person something works.
---

# Verify Heron

Run the gates in order and stop at the first red one. `H=scripts/heron/heronctl`.
Each gate ends on an exit code or a line of output named below: report those,
not an impression.

## 1. Doctor (read-only, 5 s)

```
$H status
```

Green: exit 0, every computer `up` three times, `vs checkout` is `same`
after a deploy. Read every note under the table: a `keeper.json overrides`
or `taken down by heronctl kill` line means an earlier run did not clean up
(`$H test-mode off`, `$H revive`). `UNREACHABLE`: fix ssh to that computer
first; the gates below would silently skip it.

## 2. Keeper logic (no computers, 1 s)

```
python3 heron/keeper/test_keeper.py
```

Green: `OK`. A change to leasing, takeover, fail-back or model fallback
needs its scenario here before it is deployed: see `Sim` in that file.

## 3. Deploy

```
$H diff && echo "nothing to deploy"
$H deploy
```

Green: every computer prints `changed …` or `already current`, none
`FAILED` or `KEEPER IS NOT RUNNING`, and `$H diff` then exits 0. If `diff`
lists a file you did not change, stop: another checkout is newer.

## 4. Features

One file per feature in [`features/`](features/README.md), each with the
commands and the output that proves it. Run the ones your change touches;
run all of them before a release to the phone.

## 5. Cleanup

```
$H revive; $H test-mode off; $H chat cleanup
$H tasks --all | grep '^dev-'        # then: $H task rm <id> for each
$H status
```

Green: `status` exits 0 with no `overrides` and no `taken down` note.
Evidence (the command output you captured) stays in your report; nothing
on the computers needs keeping.

## Limits of this skill

- The dev chat and dev tasks prove the agent, the extension, the runner and
  the scheduler. Only `--live` or the app proves the person's own chat, and
  the keeper's chat takeover acts on that chat alone: see
  [`features/failover.md`](features/failover.md) before touching it.
- Each computer's working models differ. An `ERRORS: 429` or `401` in a
  reply is that computer's login or limit, not your change: retry `--on`
  another computer.
