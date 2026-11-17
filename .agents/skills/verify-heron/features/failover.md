# Failover

Two separate things move when a computer goes down: its tasks (to another
computer, resuming from their log and files) and the one Heron chat (to the
first online computer in priority order).

## Sub-features

- A dead computer's task moves after `offline_secs` and resumes without redoing finished steps
- A returning computer re-runs nothing
- The chat is taken over; the preferred computer takes it back when quiet
- A model limit falls back to the next working model

## How to get to it (user POV)

A laptop lid closes or a machine loses power mid-conversation; the person
keeps typing and expects an answer.

## Driving it with heronctl

Logic first, always: `python3 heron/keeper/test_keeper.py` covers every
sub-feature above in a second. The live run below is the end-of-work proof.

Task failover (safe while the person uses Heron, as long as you kill a
computer that is not hosting the chat: check `$H chat where`):

```
T=$($H task start "Failover" 'Use only bash, in your work folder. Step 1: run `echo "step1 $(hostname)" >> steps.txt`. Step 2: run `sleep 200`. Step 3: run `echo "step3 $(hostname)" >> steps.txt`. If steps.txt already has a step1 line when you start, skip step 1. Report the contents of steps.txt.')
$H wait "task $T: started" --timeout 90      # note which computer: VICTIM
$H task show $T                              # until the log shows the sleep running
$H kill VICTIM --hard
$H wait "task $T: .*moved to" "task $T: started request 1 attempt 2 \(resuming\)" --timeout 240
$H revive VICTIM
$H task show $T                              # until state done
```

Proof: the `moved to` and `attempt 2 (resuming)` lines, then `state done`
with a result showing one `step1 <victim>` line and one `step3 <new
computer>` line. After `revive`, `$H logs --hosts VICTIM` shows no new
`started` line for `$T`.

With default timings the move takes about 2 minutes. `$H test-mode on`
shortens `offline_secs` to 45 s on every computer; turn it off afterwards.

Chat takeover (moves the person's real chat: only when they are not using
it, and say so first): `$H test-mode on`, `$H kill <chat host> --hard`,
`$H wait 'took over'`, `$H chat send "which computer are you on?" --live
--wait 200 --on <a live computer>`, `$H revive`, then `$H wait 'took .* back'`
(after `failback_quiet_secs`), `$H test-mode off`.

## Gotchas

- `test-mode` must be the same on every computer; `status` flags a mismatch.
  Under 45 s a healthy computer looks offline (Zeron's heartbeat is ~15 s).
- `kill` without `--hard` stops only Zeron: the machine's runs keep going,
  which is the "dropped off the network" case, not the "lost power" one.
- Killing and reviving leaves `*.sync-conflict*` files in `~/.heron/threads`;
  `status` counts them.
- On the MacBook, Zeron is the desktop app: `kill mbp` quits it for the
  person using that Mac.
