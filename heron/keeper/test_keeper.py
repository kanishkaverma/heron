#!/usr/bin/env python3
"""heron-keeper's decisions, on three simulated computers, in under a second.

  python3 heron/keeper/test_keeper.py            (or: python3 -m unittest -v heron/keeper/test_keeper.py)

No Zeron, no ssh, no waiting: the task board is a temp folder (what Syncthing
would keep the same everywhere), each computer has its own runs folder, and
starting or killing a run only records that it happened. A scenario that
takes minutes against real computers (cut one, wait out offline_secs) is a
few `tick`s here. Use it for every change to leasing, failover, takeover and
model fallback; keep the real kill tests (`heronctl kill`) for the end.
"""
import datetime
import importlib.util
import json
import os
import shutil
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("heron_keeper", os.path.join(HERE, "heron-keeper.py"))
K = importlib.util.module_from_spec(spec)
spec.loader.exec_module(K)

CFG = dict(K.DEFAULTS)
AIR, ZEPH, MBP = CFG["priority"]
IDS = {AIR: "id-air", ZEPH: "id-zeph", MBP: "id-mbp"}


def iso(secs_ago):
    t = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=secs_ago)
    return t.isoformat().replace("+00:00", "Z")


class Sim:
    """Three computers sharing one task board."""

    def __init__(self):
        self.tmp = tempfile.mkdtemp(prefix="heron-keeper-test-")
        K.THREADS = os.path.join(self.tmp, "threads")
        os.makedirs(K.THREADS)
        self.up = {AIR, ZEPH, MBP}      # computers whose Zeron the others can see
        self.alive = set()              # pids of runs that are still going
        self.pid = 1000
        self.started, self.killed, self.logs = [], [], []
        self.now = None                 # the computer whose keeper is ticking
        K.log = lambda *parts: self.logs.append(" ".join(str(p) for p in parts))
        K.pid_alive = lambda pid: pid in self.alive
        K.start_run = self._start
        K.kill_run = self._kill

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    # what the keeper would do to the machine, recorded instead
    def _start(self, t, attempt, resume):
        self.pid += 1
        self.alive.add(self.pid)
        os.makedirs(K.RUNS, exist_ok=True)
        req = t["request"]["req"]
        K.write_json(os.path.join(K.RUNS, t["id"] + ".json"), {"pid": self.pid, "req": req, "attempt": attempt})
        self.started.append((self.now, t["id"], attempt, resume))
        prev = K.read_json(os.path.join(t["dir"], "status.json"), {})
        restarts = prev.get("restarts", 0) + 1 if prev.get("req") == req and prev.get("attempt") == attempt else 0
        self.status(t["id"], "running", req=req, attempt=attempt, restarts=restarts)  # as runner.mjs does

    def _kill(self, tid, run, sig):
        self.alive.discard(run["pid"])
        self.killed.append((self.now, tid, sig))

    # the world
    def devices(self, me):
        return [{"id": IDS[n], "name": n, "local": n == me} for n in (AIR, ZEPH, MBP)]

    def tick(self, name, zeron=True):
        """One keeper tick on `name`. zeron=False: its own Zeron is down."""
        self.now = name
        K.RUNS = os.path.join(self.tmp, "runs-" + name)
        me = {"id": IDS[name], "name": name, "local": True}
        if zeron:
            K.schedule(self.devices(name), me, lambda d: d["name"] in self.up or d["name"] == name, CFG)
        else:
            K.schedule(None, me, None, CFG)

    def tick_all(self, times=2):
        for _ in range(times):
            for name in (AIR, ZEPH, MBP):
                if name in self.up:
                    self.tick(name)

    def die(self, name):
        """The computer loses power: gone from Zeron, its runs with it."""
        self.up.discard(name)
        runs = os.path.join(self.tmp, "runs-" + name)
        for f in os.listdir(runs) if os.path.isdir(runs) else []:
            self.alive.discard(K.read_json(os.path.join(runs, f), {}).get("pid"))

    def task(self, tid, req=1):
        d = os.path.join(K.THREADS, tid)
        os.makedirs(d, exist_ok=True)
        K.write_json(os.path.join(d, "spec.json"), {"id": tid, "title": tid})
        K.write_json(os.path.join(d, "request.json"), {"req": req})

    def read(self, tid, name):
        return K.read_json(os.path.join(K.THREADS, tid, name + ".json"), {})

    def status(self, tid, state, **more):
        prev = self.read(tid, "status")
        K.write_json(os.path.join(K.THREADS, tid, "status.json"), {**prev, **more, "state": state})

    def finish(self, tid):
        """The task's run ends by itself, as runner.mjs would report it."""
        owner = self.read(tid, "lease")["ownerName"]
        run = K.read_json(os.path.join(self.tmp, "runs-" + owner, tid + ".json"), {})
        self.alive.discard(run.get("pid"))
        self.status(tid, "done")

    def owner(self, tid):
        return self.read(tid, "lease").get("ownerName")

    def starts(self, tid):
        return [(who, attempt, resume) for who, t, attempt, resume in self.started if t == tid]


class Tasks(unittest.TestCase):
    def setUp(self):
        self.s = Sim()
        self.addCleanup(self.s.close)

    def test_leader_spreads_tasks_over_the_online_computers(self):
        s = self.s
        for i in range(3):
            s.task("t%d" % i)
        s.tick_all()
        self.assertEqual(sorted(s.owner("t%d" % i) for i in range(3)), sorted([AIR, ZEPH, MBP]))
        self.assertEqual(len(s.started), 3)
        self.assertTrue(all(attempt == 1 and not resume for _, _, attempt, resume in s.started))

    def test_only_the_leader_leases(self):
        s = self.s
        s.task("t")
        s.tick(ZEPH)
        s.tick(MBP)
        self.assertEqual(s.read("t", "lease"), {})
        s.tick(AIR)
        self.assertEqual(s.owner("t"), AIR)

    def test_next_computer_leads_when_the_leader_is_gone(self):
        s = self.s
        s.die(AIR)
        s.task("t")
        s.tick(MBP)
        self.assertEqual(s.read("t", "lease"), {})
        s.tick(ZEPH)
        self.assertIn(s.owner("t"), (ZEPH, MBP))

    def test_a_task_is_started_once(self):
        s = self.s
        s.task("t")
        s.tick_all(times=5)
        self.assertEqual(len(s.starts("t")), 1)

    def test_a_dead_computers_task_moves_and_resumes(self):
        s = self.s
        s.task("a")
        s.task("b")
        s.tick_all()
        victim = s.owner("b")
        self.assertNotEqual(victim, AIR)  # air leads and took the first task
        s.die(victim)
        s.tick_all(times=2)
        lease = s.read("b", "lease")
        self.assertNotEqual(lease["ownerName"], victim)
        self.assertEqual((lease["attempt"], lease["movedFrom"]), (2, victim))
        self.assertEqual(s.starts("b")[-1], (lease["ownerName"], 2, True))
        self.assertEqual(len(s.starts("a")), 1)  # the survivor's own task is left alone

    def test_the_leader_dying_moves_its_task_too(self):
        s = self.s
        s.task("t")
        s.tick_all()
        self.assertEqual(s.owner("t"), AIR)
        s.die(AIR)
        s.tick_all(times=2)
        self.assertIn(s.owner("t"), (ZEPH, MBP))
        self.assertEqual(s.starts("t")[-1][1:], (2, True))

    def test_a_returning_computer_stops_its_stale_run_and_reruns_nothing(self):
        s = self.s
        s.task("t")
        s.tick_all()
        # air's Zeron drops off the network but the machine (and its run) keeps going
        s.up.discard(AIR)
        s.tick_all(times=2)
        self.assertNotEqual(s.owner("t"), AIR)
        s.up.add(AIR)
        s.tick_all(times=2)
        self.assertIn((AIR, "t", "SIGKILL"), s.killed)
        self.assertEqual([who for who, _, _ in s.starts("t")].count(AIR), 1)
        self.assertNotEqual(s.owner("t"), AIR)

    def test_a_finished_task_is_never_run_again(self):
        s = self.s
        s.task("t")
        s.tick_all()
        s.finish("t")
        s.die(s.owner("t"))
        s.tick_all(times=3)
        self.assertEqual(len(s.starts("t")), 1)
        self.assertEqual(s.read("t", "lease")["attempt"], 1)

    def test_a_follow_up_request_runs_again(self):
        s = self.s
        s.task("t")
        s.tick_all()
        s.finish("t")
        s.task("t", req=2)
        s.tick_all(times=2)
        self.assertEqual(s.read("t", "lease")["req"], 2)
        self.assertEqual(len(s.starts("t")), 2)

    def test_stop_ends_the_run_and_it_stays_stopped(self):
        s = self.s
        s.task("t")
        s.tick_all()
        K.write_json(os.path.join(K.THREADS, "t", "stop.json"), {"req": 1})
        s.tick_all(times=3)
        self.assertEqual([k[2] for k in s.killed], ["SIGTERM"])
        self.assertEqual(len(s.starts("t")), 1)

    def test_a_run_that_dies_is_resumed_then_given_up_on(self):
        s = self.s
        s.task("t")
        s.tick(AIR)
        s.tick(AIR)
        for _ in range(K.MAX_RESTARTS + 2):
            s.alive.clear()  # the run's process dies; status still says running
            s.tick(AIR)
        self.assertEqual(len(s.starts("t")), 1 + K.MAX_RESTARTS)
        self.assertTrue(all(resume for _, _, resume in s.starts("t")[1:]))
        self.assertEqual(s.read("t", "status")["state"], "failed")

    def test_with_zeron_down_a_keeper_still_runs_what_is_leased_to_it(self):
        s = self.s
        s.task("t")
        s.tick(AIR)
        s.tick(AIR)  # leased on one tick, started on the next
        s.alive.clear()
        s.tick(AIR, zeron=False)
        self.assertEqual(len(s.starts("t")), 2)
        s.task("new")
        s.tick(AIR, zeron=False)
        self.assertEqual(s.read("new", "lease"), {})  # but leases nothing blind

    def test_nothing_moves_when_no_other_computer_is_up(self):
        s = self.s
        s.task("t")
        s.tick_all()
        s.die(ZEPH)
        s.die(MBP)
        s.tick_all(times=2)
        self.assertEqual((s.owner("t"), s.read("t", "lease")["attempt"]), (AIR, 1))

    def test_load_counts_running_tasks_when_placing_a_new_one(self):
        s = self.s
        s.die(MBP)
        s.task("a")
        s.task("b")
        s.tick_all()
        s.finish("a")
        busy = s.owner("b")
        s.task("c")
        s.tick_all()
        self.assertNotEqual(s.owner("c"), busy)


class FakeMcp:
    def __init__(self, chats, messages=()):
        self.chats, self.messages, self.calls = chats, list(messages), []

    def tool(self, name, args=None):
        self.calls.append((name, args or {}))
        if name == "list_chats":
            return {"chats": self.chats}
        if name == "get_chat":
            return {"harness": "pi", "reasoning": None}
        if name == "read_chat":
            return {"messages": self.messages}
        if name == "create_chat":
            return {"chatId": "new-chat"}
        return {}

    def did(self, name):
        return [a for n, a in self.calls if n == name]


class Chat(unittest.TestCase):
    """keep_chat: who hosts the one Heron chat, and on which model."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="heron-keeper-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        K.THREADS = os.path.join(self.tmp, "threads")
        K.MODELS = os.path.join(self.tmp, "models.json")
        K.MODEL_STATE = os.path.join(self.tmp, "model-state.json")
        K.write_json(K.MODELS, ["claude", "gpt"])
        self.ipc, self.logs = [], []
        K.ipc = lambda method, params: self.ipc.append(params)
        K.log = lambda *parts: self.logs.append(" ".join(str(p) for p in parts))

    def chat(self, host, **more):
        return {"id": "chat-1", "title": "Heron", "cwd": "~/heron", "deviceId": IDS[host], "model": "claude",
                "sessionStatus": "idle", "lastMessageAt": iso(3600), "createdAt": "2026-01-01", **more}

    def keep(self, me, mcp, up=(AIR, ZEPH, MBP)):
        devices = [{"id": IDS[n], "name": n, "local": n == me, "lastSeenAt": iso(0 if n in up else 500)} for n in (AIR, ZEPH, MBP)]
        K.keep_chat(mcp, CFG, devices, devices[[AIR, ZEPH, MBP].index(me)], lambda d: d["name"] in up)
        return [p for p in self.ipc if p.get("op") == "setChatHost"]

    def test_first_online_computer_takes_over_a_dead_hosts_chat(self):
        self.assertEqual(self.keep(MBP, FakeMcp([self.chat(AIR)]), up=(ZEPH, MBP)), [])
        moved = self.keep(ZEPH, FakeMcp([self.chat(AIR)]), up=(ZEPH, MBP))
        self.assertEqual([m["deviceId"] for m in moved], [IDS[ZEPH]])

    def test_last_computer_takes_over_when_it_is_the_only_one_left(self):
        moved = self.keep(MBP, FakeMcp([self.chat(AIR)]), up=(MBP,))
        self.assertEqual([m["deviceId"] for m in moved], [IDS[MBP]])

    def test_nobody_touches_a_chat_whose_host_is_fine_and_preferred(self):
        for me in (ZEPH, MBP):
            self.assertEqual(self.keep(me, FakeMcp([self.chat(AIR)])), [])

    def test_preferred_computer_takes_a_quiet_chat_back(self):
        moved = self.keep(AIR, FakeMcp([self.chat(MBP)]))
        self.assertEqual([m["deviceId"] for m in moved], [IDS[AIR]])

    def test_a_busy_or_recent_chat_is_not_taken_back(self):
        self.assertEqual(self.keep(AIR, FakeMcp([self.chat(MBP, sessionStatus="working")])), [])
        self.assertEqual(self.keep(AIR, FakeMcp([self.chat(MBP, lastMessageAt=iso(30))])), [])

    def test_only_the_first_computer_takes_it_back(self):
        self.assertEqual(self.keep(ZEPH, FakeMcp([self.chat(MBP)])), [])

    def test_the_first_online_computer_makes_the_chat_when_there_is_none(self):
        late = FakeMcp([])
        self.keep(ZEPH, late)
        self.assertEqual(late.did("create_chat"), [])
        first = FakeMcp([])
        self.keep(AIR, first)
        self.assertEqual([c["title"] for c in first.did("create_chat")], ["Heron"])

    def test_other_chats_in_the_folder_are_ignored(self):
        dev = self.chat(AIR, id="dev", title="Heron dev")
        child = self.chat(AIR, id="child", parentChatId="chat-1")
        mcp = FakeMcp([dev, child])
        self.keep(ZEPH, mcp, up=(ZEPH, MBP))
        self.assertEqual(self.ipc, [])

    def test_an_archived_heron_chat_is_brought_back_not_duplicated(self):
        mcp = FakeMcp([self.chat(AIR, archived=True)])
        self.keep(AIR, mcp)
        self.assertEqual(mcp.did("archive_chat"), [{"chat": "chat-1", "archived": False}])
        self.assertEqual(mcp.did("create_chat"), [])

    def test_the_host_puts_the_chat_on_its_own_working_model(self):
        self.keep(AIR, FakeMcp([self.chat(AIR, model="something-else")]))
        self.assertEqual([p["config"]["model"] for p in self.ipc], ["claude"])

    def test_a_limit_error_falls_back_to_the_next_model_once(self):
        errored = self.chat(AIR, sessionStatus="errored")
        bad = [{"id": "m1", "role": "assistant", "errors": ["429 rate limit reached"]}]
        mcp = FakeMcp([errored], bad)
        self.keep(AIR, mcp)
        self.assertEqual({p["config"]["model"] for p in self.ipc}, {"gpt"})
        self.assertEqual(len(mcp.did("send_message")), 1)
        # the same failed message on the next tick: handled already
        again = FakeMcp([self.chat(AIR, sessionStatus="errored", model="gpt")], bad)
        self.keep(AIR, again)
        self.assertEqual(again.did("send_message"), [])
        self.assertEqual(K.heron_model(), "gpt")  # and it stays on the fallback for the hour

    def test_an_ordinary_error_does_not_switch_models(self):
        mcp = FakeMcp([self.chat(AIR, sessionStatus="errored")], [{"id": "m1", "role": "assistant", "errors": ["tool crashed"]}])
        self.keep(AIR, mcp)
        self.assertEqual(self.ipc, [])
        self.assertEqual(mcp.did("send_message"), [])


if __name__ == "__main__":
    unittest.main(verbosity=2, warnings="ignore")
