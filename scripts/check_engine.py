"""Scenario checks for the alert engine, driven through `pay-attention simulate`.

Every scenario runs in a throwaway HOME with TZ=UTC; times are unix ms around 2026-10-14.
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

BIN = sys.argv[1]
TMP = tempfile.mkdtemp(prefix="pa-engine.")
X, Y = "/dev/ttys901", "/dev/ttys902"


def at(hhmm):
    return int(datetime.strptime(f"2026-10-14 {hhmm}", "%Y-%m-%d %H:%M").replace(tzinfo=timezone.utc).timestamp() * 1000)


NOON = at("12:00")


def home(name, config=""):
    h = os.path.join(TMP, name)
    os.makedirs(os.path.join(h, ".config/pay-attention"), exist_ok=True)
    with open(os.path.join(h, ".config/pay-attention/config.toml"), "w") as f:
        f.write(config)
    return h


def row(key, state, tty=X, since=1, surface="terminal", reason=None, name=None, detail=None):
    harness = key.split(":")[0]
    return {"key": key, "harness": harness, "surface": surface, "pid": 4242, "tty": tty if surface == "terminal" else None,
            "name": name or key.split(":")[1], "cwd": "/tmp/p", "state": state, "reason": reason, "detail": detail,
            "since": since, "acked": False}


def tick(now, rows, frontmost=None, idle=100, locked=False):
    return {"type": "tick", "now": now, "rows": rows, "frontmost_tty": frontmost, "idle_secs": idle, "locked": locked}


def seconds(start, secs, rows_at, **kw):
    """One tick per second for t in [0, secs]; rows_at(t) gives the rows; kw values may be callables of t."""
    out = []
    for t in range(secs + 1):
        args = {k: (v(t) if callable(v) else v) for k, v in kw.items()}
        out.append(tick(start + t * 1000, rows_at(t), **args))
    return out


def run(h, lines, tz="UTC"):
    p = subprocess.run([BIN, "simulate"], input="\n".join(json.dumps(x) for x in lines) + "\n", capture_output=True,
                       text=True, env={**os.environ, "HOME": h, "TZ": tz}, timeout=120)
    if p.returncode != 0:
        fail(f"simulate exited {p.returncode}: {p.stderr[-500:]}")
    return [json.loads(x) for x in p.stdout.splitlines() if x.strip()]


def run_streaming(h, *steps):
    """Feed each list of lines; for each callable step, wait, call it (e.g. an outside write), wait.
    simulate must read line by line."""
    p = subprocess.Popen([BIN, "simulate"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         text=True, env={**os.environ, "HOME": h, "TZ": "UTC"})
    for step in steps:
        if callable(step):
            time.sleep(1.0)
            step()
            time.sleep(1.1)
        else:
            p.stdin.write("\n".join(json.dumps(x) for x in step) + "\n")
            p.stdin.flush()
    out, err = p.communicate(timeout=60)
    if p.returncode != 0:
        fail(f"simulate exited {p.returncode}: {err[-500:]}")
    return [json.loads(x) for x in out.splitlines() if x.strip()]


def of(actions, kind):
    return [a for a in actions if a["type"] == kind]


def secs_of(actions, start):
    return [round((a["now"] - start) / 1000, 2) for a in actions]


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def check(cond, msg):
    if not cond:
        fail(msg)


def ok(msg):
    print(f"ok: {msg}")


# S1 + S2: blue rings at 0 s and 60 s, then focus with recent input acks it and the ringing stops.
a = row("claude:a", "waiting", reason="question", since=5)
acts = run(home("s1"), seconds(NOON, 200, lambda t: [a],
                               frontmost=lambda t: X if t >= 65 else None, idle=lambda t: 2 if t >= 65 else 100))
rings = of(acts, "ring")
check(secs_of(rings, NOON) == [0, 60], f"blue ring times {secs_of(rings, NOON)}, want [0, 60]")
check([r["vol"] for r in rings] == [0.3, 0.44] and all(r["color"] == "blue" and r["keys"] == ["claude:a"] for r in rings),
      f"blue rings {rings}")
acks = of(acts, "ack")
check([(x["key"], x["by"], x["now"]) for x in acks] == [("claude:a", "focus", NOON + 65_000)], f"acks {acks}")
lights = of(acts, "light")
check(lights[0]["color"] == "blue" and lights[0]["count"] == 1 and lights[-1]["color"] == "grey" and lights[-1]["count"] == 0,
      f"lights {lights}")
ev = of(acts, "event")
check([(e["key"], e["from"], e["to"], e["ts"]) for e in ev] == [("claude:a", "new", "waiting", NOON)], f"events {ev}")
ok("blue: rings at 0/60 s with rising volume; focus + recent input acks; light blue -> grey")

# S3: focus without recent input (idle 120 s) does not ack; ringing continues.
acts = run(home("s3"), seconds(NOON, 200, lambda t: [a], frontmost=lambda t: X if t >= 65 else None, idle=120))
check(not of(acts, "ack"), f"focus with stale input acked: {of(acts, 'ack')}")
check(len([r for r in of(acts, "ring") if r["now"] > NOON + 65_000]) >= 2, "rings stopped without an ack")
ok("focus with no recent input does not ack")

# S4: a session you are watching finishes its turn: acked on the tick it starts, never rings.
acts = run(home("s4"), seconds(NOON, 60, lambda t: [row("claude:b", "working" if t == 0 else "idle", tty=Y, since=1 if t == 0 else 2)],
                               frontmost=Y, idle=1))
check(not of(acts, "ring"), f"watched session rang: {of(acts, 'ring')}")
check([(x["key"], x["by"], x["now"]) for x in of(acts, "ack")] == [("claude:b", "focus", NOON + 1000)], f"acks {of(acts, 'ack')}")
ok("watched session: acked immediately, silent")

# S5: yellow waits 2 s (a 1 s blip never rings) and stops after 3 rings.
def s5(t):
    if t == 1:
        return [row("claude:y", "idle", since=2)]
    if t >= 10:
        return [row("claude:y", "idle", since=3)]
    return [row("claude:y", "working", since=1)]


acts = run(home("s5"), seconds(NOON, 400, s5))
rings = of(acts, "ring")
check(len(rings) == 3 and secs_of(rings, NOON)[0] == 12 and all(r["color"] == "yellow" for r in rings),
      f"yellow rings at {secs_of(rings, NOON)}, want 3 starting at 12")
ok("yellow: 2 s debounce, 3 rings max")

# S6: two alerts due on one tick -> one ring, top priority color, both keys advance.
acts = run(home("s6"), seconds(NOON, 5, lambda t: [row("claude:a", "waiting", since=1), row("codex:c", "error", tty=Y, reason="crashed", since=1)]))
rings = of(acts, "ring")
check(len(rings) == 1 and rings[0]["color"] == "red" and sorted(rings[0]["keys"]) == ["claude:a", "codex:c"], f"rings {rings}")
ok("one ring per tick at the top priority")

# S7: DND 23:00-08:00 drops rings; one catch-up ring when it ends (07:58 + 120 s), then the schedule resumes.
start = at("07:58")
acts = run(home("s7", 'dnd = "23:00-08:00"\n'), seconds(start, 200, lambda t: [a]))
rings = of(acts, "ring")
check(secs_of(rings, start)[:2] == [120, 160] and rings[0]["vol"] == 0.58, f"DND rings at {secs_of(rings, start)} vol {[r['vol'] for r in rings]}")
check(of(acts, "light") and of(acts, "light")[0]["now"] == start, "light did not update during DND")
ok("DND: silent, catch-up ring at window end, light still updates")

# S8: quiet for 100 s works like DND.
acts = run(home("s8", 'dnd = ""\n'), [{"type": "quiet", "now": NOON, "until": NOON + 100_000}] + seconds(NOON, 130, lambda t: [a]))
rings = of(acts, "ring")
check(secs_of(rings, NOON)[:1] == [100] and rings[0]["vol"] == 0.53, f"quiet rings at {secs_of(rings, NOON)}")
ok("quiet window: silent, catch-up at its end")

# S9: phone once per episode while away (idle >= 300 s or locked), after 120 s unacked; never in DND, for yellow, or unset.
PHONE = 'dnd = ""\nphone_command = ["/bin/echo", "{name}: {detail} ({state})"]\n'
q = row("claude:q", "waiting", reason="question", name="proj-x", detail="Which DB?", since=7)
cases = [
    ("away-idle", PHONE, q, dict(idle=400), 1),
    ("away-locked", PHONE, q, dict(idle=0, locked=True), 1),
    ("present", PHONE, q, dict(idle=10), 0),
    ("dnd", 'dnd = "00:00-23:59"\nphone_command = ["/bin/echo", "x"]\n', q, dict(idle=400), 0),
    ("unset", 'dnd = ""\n', q, dict(idle=400), 0),
    ("yellow", PHONE, row("claude:q", "idle", name="proj-x", since=7), dict(idle=400), 0),
    ("red", PHONE, row("claude:q", "error", reason="crashed", name="proj-x", detail="Which DB?", since=7), dict(idle=400), 1),
]
for name, cfg, r, kw, want in cases:
    acts = run(home("s9-" + name, cfg), seconds(NOON, 300, lambda t, r=r: [r], **kw))
    ph = of(acts, "phone")
    check(len(ph) == want, f"phone {name}: {len(ph)} calls, want {want}: {ph}")
    if want:
        check(ph[0]["now"] == NOON + 120_000 and ph[0]["key"] == "claude:q", f"phone {name} at {ph[0]}")
        if name == "away-idle":
            check(ph[0]["argv"] == ["/bin/echo", "proj-x: Which DB? (waiting)"], f"phone argv {ph[0]['argv']}")
ok("phone: once per episode at 120 s when away/locked; not present, DND, unset or yellow")

# S10: events for appear / change / disappear.
def s10(t):
    if t < 5:
        return [row("claude:e", "working", name="E", since=1)]
    if t < 10:
        return [row("claude:e", "waiting", name="E", since=2)]
    return []


acts = run(home("s10"), seconds(NOON, 12, s10))
check([(e["from"], e["to"], e["ts"] - NOON, e["name"]) for e in of(acts, "event")] ==
      [("new", "working", 0, "E"), ("working", "waiting", 5000, "E"), ("waiting", "gone", 10000, "E")], f"events {of(acts, 'event')}")
ok("events: new / change / gone")

# S11 + S13: click acks + focuses; the ack persists across engine restarts until the episode changes.
h = home("s11")
acts = run(h, seconds(NOON, 2, lambda t: [a]) + [{"type": "click", "now": NOON + 3000, "key": "claude:a"}]
           + seconds(NOON + 4000, 200, lambda t: [a]))
check([(x["key"], x["by"]) for x in of(acts, "ack")] == [("claude:a", "click")], f"click acks {of(acts, 'ack')}")
check([(x["key"], x["now"]) for x in of(acts, "focus")] == [("claude:a", NOON + 3000)], f"click focus {of(acts, 'focus')}")
check(all(r["now"] < NOON + 3000 for r in of(acts, "ring")), "rang after click ack")
acts = run(h, seconds(NOON + 300_000, 30, lambda t: [a]))
check(not of(acts, "ring"), "restarted engine rang an acked episode")
acts = run(h, seconds(NOON + 400_000, 5, lambda t: [row("claude:a", "waiting", reason="question", since=6)]))
check(len(of(acts, "ring")) == 1, "a new episode after a persisted ack did not ring")
ok("click ack + focus; acks persist across restarts; new episode rings")

# S12: hotkey order: blue (oldest first), then red, then yellow; wraps.
rows12 = [row("claude:y1", "idle", since=10), row("claude:b1", "waiting", since=100), row("claude:b2", "waiting", since=50),
          row("claude:r1", "error", reason="crashed", since=70), row("claude:w1", "working", since=1)]
acts = run(home("s12"), [tick(NOON, rows12)] + [{"type": "hotkey", "now": NOON + 1000 + i} for i in range(5)])
check([x["key"] for x in of(acts, "focus")] == ["claude:b2", "claude:b1", "claude:r1", "claude:y1", "claude:b2"],
      f"hotkey order {[x['key'] for x in of(acts, 'focus')]}")
ok("hotkey: blue oldest-first, red, yellow, wrap")

# S14: headless rows ring only in error; unknown never.
hl = [row("claude:h1", "working", surface="headless"), row("claude:h2", "idle", surface="headless"),
      row("claude:h3", "waiting", surface="headless"), row("claude:u", "unknown")]
acts = run(home("s14"), seconds(NOON, 30, lambda t: hl))
check(not of(acts, "ring"), f"headless/unknown rang: {of(acts, 'ring')}")
acts = run(home("s14b"), seconds(NOON, 3, lambda t: hl + [row("claude:h4", "error", surface="headless", reason="crashed")]))
check([(r["color"], r["keys"]) for r in of(acts, "ring")] == [("red", ["claude:h4"])], f"headless error rings {of(acts, 'ring')}")
ok("headless: only errors ring; unknown never")

# S15: an ack written by another process (pay-attention ack / acks.json) stops a running engine.
h = home("s15")
def outside_ack():
    acks = os.path.join(h, ".local/state/pay-attention/acks.json")
    os.makedirs(os.path.dirname(acks), exist_ok=True)
    subprocess.run([BIN, "ack", "claude:a"], env={**os.environ, "HOME": h}, capture_output=True)
    if not os.path.exists(acks) or "claude:a" not in open(acks).read():
        with open(acks, "w") as f:  # no registry in this HOME, so write the record the way `ack` stores it
            json.dump({"claude:a": {"since": a["since"], "state": a["state"], "reason": a["reason"]}}, f)
acts = run_streaming(h, seconds(NOON, 10, lambda t: [a]), outside_ack, seconds(NOON + 11_000, 200, lambda t: [a]))
check(not [r for r in of(acts, "ring") if r["now"] > NOON + 11_000], f"rang after an outside ack: {secs_of(of(acts, 'ring'), NOON)}")
check(of(acts, "light")[-1]["color"] == "grey", f"light after an outside ack: {of(acts, 'light')[-1]}")
ok("an ack from another process stops a running engine")

# S16: an unwritable state dir never crashes the engine; the click still acks in memory.
h = home("s16")
sd = os.path.join(h, ".local/state/pay-attention")
os.makedirs(sd, exist_ok=True)
os.chmod(sd, 0o500)
try:
    acts = run(h, seconds(NOON, 2, lambda t: [a]) + [{"type": "click", "now": NOON + 3000, "key": "claude:a"}]
               + seconds(NOON + 4000, 120, lambda t: [a]))
finally:
    os.chmod(sd, 0o700)
check(of(acts, "ack") and not [r for r in of(acts, "ring") if r["now"] > NOON + 3000], "unwritable acks: click did not hold")
ok("unwritable state dir: no crash, click ack holds in memory")

# S17: spring-forward DST (America/New_York, 2026-03-08): DND 22:00-07:00 ends at 07:00 EDT = 11:00 UTC.
start = int(datetime(2026, 3, 8, 6, 30, tzinfo=timezone.utc).timestamp() * 1000)  # 01:30 EST, inside DND
lines = [tick(start + i * 10_000, [a]) for i in range(0, 6 * 360 + 1)]
acts = run(home("s17", 'dnd = "22:00-07:00"\n'), lines, tz="America/New_York")
rings = of(acts, "ring")
want = int(datetime(2026, 3, 8, 11, 0, tzinfo=timezone.utc).timestamp() * 1000)
check(rings and rings[0]["now"] == want, f"DST: first ring at {rings[0]['now'] if rings else None}, want {want} (07:00 EDT)")
ok("DND end across spring-forward DST")

# S18: the wall clock jumps back an hour: existing alerts keep ringing on schedule.
jump = NOON - 3_600_000
acts = run(home("s18"), seconds(NOON, 5, lambda t: [a]) + seconds(jump, 120, lambda t: [a]))
after = [r for r in of(acts, "ring") if jump <= r["now"] <= jump + 120_000]
check(len(after) >= 1, f"no ring within 120 s after the clock went back an hour: {of(acts, 'ring')}")
ok("backward clock jump does not silence alerts")

# S19: an acked error that then crashes (same since, new reason) is a new red alert.
err = row("codex:x", "error", reason="api_error", since=9)
crashed = row("codex:x", "error", reason="crashed", since=9)
acts = run(home("s19"), seconds(NOON, 2, lambda t: [err]) + [{"type": "click", "now": NOON + 3000, "key": "codex:x"}]
           + seconds(NOON + 4000, 10, lambda t: [crashed]))
check([r for r in of(acts, "ring") if r["now"] >= NOON + 4000 and r["color"] == "red"], "crash after an acked error did not ring")
check(of(acts, "light")[-1]["color"] == "red", f"light after crash: {of(acts, 'light')[-1]}")
ok("crash after an acked error rings red")

# S20: shortening quiet moves the catch-up ring to the new end.
acts = run(home("s20", 'dnd = ""\n'), [{"type": "quiet", "now": NOON, "until": NOON + 600_000}] + seconds(NOON, 5, lambda t: [a])
           + [{"type": "quiet", "now": NOON + 6000, "until": NOON + 30_000}] + seconds(NOON + 6000, 60, lambda t: [a]))
check(secs_of(of(acts, "ring"), NOON)[:1] == [30], f"shortened quiet: rings at {secs_of(of(acts, 'ring'), NOON)}, want first at 30")
ok("shortened quiet: catch-up at the new end")

# S21: phone once per episode even if the row briefly disappears; placeholders substituted once.
qq = row("claude:q", "waiting", reason="question", name="proj-x", detail="use {state} here", since=7)
acts = run(home("s21", PHONE), seconds(NOON, 300, lambda t: [] if 131 <= t <= 140 else [qq], idle=400))
ph = of(acts, "phone")
check(len(ph) == 1, f"phone calls {len(ph)} after the row blinked, want 1")
check(ph[0]["argv"] == ["/bin/echo", "proj-x: use {state} here (waiting)"], f"placeholder argv {ph[0]['argv']}")
ok("phone dedup survives a blinking row; placeholders substituted in one pass")

# S22: an episode keeps its timing while its row is briefly missing (rings, volume, phone, yellow's 3-ring cap),
# and is forgotten after an hour away.
steady = run(home("s22-steady", PHONE), seconds(NOON, 300, lambda t: [qq], idle=400))
blinks = {round((r["now"] - NOON) / 1000) + 5 for r in of(steady, "ring")}
acts = run(home("s22", PHONE), seconds(NOON, 300, lambda t: [] if t in blinks else [qq], idle=400))
check([(r["now"], r["vol"]) for r in of(acts, "ring")] == [(r["now"], r["vol"]) for r in of(steady, "ring")],
      f"blinking row: rings {secs_of(of(acts, 'ring'), NOON)} want {secs_of(of(steady, 'ring'), NOON)}")
check([p["now"] - NOON for p in of(acts, "phone")] == [120_000], f"blinking row: phone {of(acts, 'phone')}")
y = row("claude:y", "idle", since=3)
acts = run(home("s22-yellow"), seconds(NOON, 400, lambda t: [] if t in (10, 100, 200) else [y]))
check(len(of(acts, "ring")) == 3, f"blinking yellow rang {len(of(acts, 'ring'))} times, want 3")
acts = run(home("s22-forget"), seconds(NOON, 5, lambda t: [a]) + [tick(NOON + k * 60_000, []) for k in range(1, 121)]
           + seconds(NOON + 7_300_000, 5, lambda t: [a]))
back = [r for r in of(acts, "ring") if r["now"] >= NOON + 7_300_000]
check(back and back[0]["now"] == NOON + 7_300_000 and back[0]["vol"] == 0.3, f"episode back after 2 h: rings {back}")
ok("episode timing survives a missing row; forgotten after an hour away")

# S23: an acks.json that can't be read keeps the acks already loaded, and is retried once readable (same mtime).
h = home("s23")
acks23 = os.path.join(h, ".local/state/pay-attention/acks.json")
def unreadable_ack():
    os.makedirs(os.path.dirname(acks23), exist_ok=True)
    with open(acks23, "w") as f:
        json.dump({"claude:a": {"since": a["since"], "state": a["state"], "reason": a["reason"]}}, f)
    os.chmod(acks23, 0)
acts = run_streaming(h, seconds(NOON, 10, lambda t: [a]), unreadable_ack, seconds(NOON + 11_000, 10, lambda t: [a]),
                     lambda: os.chmod(acks23, 0o600), seconds(NOON + 22_000, 200, lambda t: [a]))
check(not [r for r in of(acts, "ring") if r["now"] >= NOON + 22_000], f"rang after acks.json became readable: {secs_of(of(acts, 'ring'), NOON)}")
ok("an unreadable acks.json is retried until it loads")

# S24: fall-back DST (America/New_York, 2026-11-01, 02:00 EDT -> 01:00 EST): DND 01:30-02:30. An alert seen at 01:59:50 EDT
# rings at 01:00 EST (06:00 UTC), when local time leaves the window, not at the window's predicted end.
start = int(datetime(2026, 11, 1, 5, 59, 50, tzinfo=timezone.utc).timestamp() * 1000)
acts = run(home("s24", 'dnd = "01:30-02:30"\n'), seconds(start, 60, lambda t: [a]), tz="America/New_York")
want = int(datetime(2026, 11, 1, 6, 0, tzinfo=timezone.utc).timestamp() * 1000)
check(of(acts, "ring") and of(acts, "ring")[0]["now"] == want, f"fall-back DST: rings at {secs_of(of(acts, 'ring'), start)}, want first at 10")
ok("DND is checked against the local clock every tick (fall-back DST)")

# S25: acking a crash from a stale snapshot never deletes the session's newer record, and a key can't point outside the
# state dir.
h = home("s25")
sess = os.path.join(h, ".local/state/pay-attention/sessions")
os.makedirs(sess, exist_ok=True)
live = os.path.join(sess, "codex-cx.json")
with open(live, "w") as f:  # the session came back after the engine's last tick: a new record, process alive (headless)
    json.dump({"session_id": "cx", "harness": "codex", "pid": None, "proc_start": None, "cwd": "/tmp/p",
               "transcript_path": None, "state": "working", "reason": None, "detail": None, "since": 50}, f)
victim = os.path.join(TMP, "victim-data.json")
with open(victim, "w") as f:
    f.write("{}")
evil = f"{TMP}/victim:data"
run(h, [tick(NOON, [row("codex:cx", "error", reason="crashed", since=9), row(evil, "error", reason="crashed", since=9)]),
        {"type": "click", "now": NOON + 1000, "key": "codex:cx"}, {"type": "click", "now": NOON + 2000, "key": evil}])
check(os.path.exists(live), "acking a stale crash deleted the session's newer record")
check(os.path.exists(victim), "a crafted key deleted a file outside the state dir")
ok("a crash ack deletes only the crashed record, only inside the state dir")

print("ENGINE CHECKS PASSED")
