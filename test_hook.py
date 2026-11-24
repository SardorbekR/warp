"""Tests the WarpOss permission fallback hook against Warp's plugin (warp 2.4.0),
Claude Code 2.1.331's terminalSequence check, and WarpOss's OSC 777 parsing."""
import json, os, re, subprocess, sys

SHIM = sys.argv[1]
PLUGIN = sys.argv[2]  # .../plugins/warp/scripts/on-permission-request.sh
BASH = sys.argv[3] if len(sys.argv) > 3 else "bash"
LIMIT = 4096
ENV = {
    "PATH": os.environ["PATH"],
    "HOME": os.environ.get("HOME", "/tmp"),
    "LANG": "en_US.UTF-8",
    "WARP_CLI_AGENT_PROTOCOL_VERSION": "1",
    "WARP_CLIENT_VERSION": "v0.2026.11.20.07.02.oss_0",
    "CLAUDE_CODE_VERSION": "2.1.331",
}

def run(script, payload, env=ENV, raw=None):
    data = raw if raw is not None else json.dumps(payload)
    p = subprocess.run([BASH, script], input=data.encode(), env=env, capture_output=True, timeout=60)
    assert p.returncode == 0, (script, p.returncode, p.stderr.decode())
    out = p.stdout.decode().strip()
    return json.loads(out)["terminalSequence"] if out else None

def claude_accepts(seq):
    """Claude Code 2.1.331 Qjr(): null when too long or not an allowed OSC."""
    if not seq or len(seq.encode("utf-8")) > LIMIT:
        return False
    i = 0
    while i < len(seq):
        if seq[i] == "\x07":
            i += 1; continue
        if seq[i] != "\x1b" or seq[i + 1:i + 2] != "]":
            return False
        h = i + 2; end = -1; k = 0
        while h < len(seq):
            if seq[h] == "\x07": end, k = h, 1; break
            if seq[h] == "\x1b" and seq[h + 1:h + 2] == "\\": end, k = h, 2; break
            if seq[h] == "\x1b": return False
            h += 1
        if end == -1: return False
        body = seq[i + 2:end]
        ps = body.split(";", 1)[0]
        if not ps.isdigit() or int(ps) not in (0, 1, 2, 9, 99, 777): return False
        i = end + k
    return True

def warposs_event(seq, max_params=16):
    """vte 0.13 (warpdotdev 4b399c8) OSC splitting + WarpOss's OSC 777 handler."""
    m = re.match(r"\x1b\](.*)\x07$", seq, re.S)
    assert m
    raw = m.group(1)
    parts = raw.split(";")
    if len(parts) > max_params:
        # vte records at most 16 params; everything after the 16th separator is lost.
        parts = parts[:max_params]
    assert parts[0] == "777" and parts[1] == "notify"
    body = ";".join(parts[3:]).strip()
    try:
        return json.loads(body)
    except json.JSONDecodeError:
        return None

def payload(tool, tool_input):
    return {"session_id": "7c67547d-53f0-4698-970f-5f4468c2fbab", "cwd": "/Users/me/SideProjects/Coach-Journal",
            "hook_event_name": "PermissionRequest", "tool_name": tool, "tool_input": tool_input,
            "permission_mode": "plan", "transcript_path": "/Users/me/.claude/projects/x/7c.jsonl"}

failures = 0
def check(name, cond, detail=""):
    global failures
    print(("PASS " if cond else "FAIL ") + name + (f"  [{detail}]" if detail and not cond else ""))
    failures += 0 if cond else 1

# 1. A long plan with many semicolons, like the 10,598-byte one.
plan = ("## Plan\n" + "- Adapt the iPad layout; keep the duo view; test it; ship it.\n" * 170)
p1 = payload("ExitPlanMode", {"plan": plan})
plug = run(PLUGIN, p1); shim = run(SHIM, p1)
check("long plan: plugin message is over 4096 bytes", len(plug.encode()) > LIMIT, len(plug.encode()))
check("long plan: Claude drops the plugin message", not claude_accepts(plug))
check("long plan: hook sends a short copy", shim is not None)
check("long plan: Claude accepts the short copy", shim is not None and claude_accepts(shim), shim and len(shim.encode()))
ev = shim and warposs_event(shim)
check("long plan: WarpOss 9.3 parses it as permission_request for ExitPlanMode",
      bool(ev) and ev["event"] == "permission_request" and ev["tool_name"] == "ExitPlanMode", ev)
check("long plan: same session, cwd and project as the plugin",
      bool(ev) and ev["session_id"] == p1["session_id"] and ev["cwd"] == p1["cwd"] and ev["project"] == "Coach-Journal", ev)
check("long plan: summary starts like the plugin's", bool(ev) and ev["summary"].startswith("Wants to run ExitPlanMode: "), ev and ev["summary"])
check("long plan: short copy has at most 12 semicolons", shim is not None and shim.count(";") - 3 <= 12, shim and shim.count(";"))

# 2. A short plan: the plugin's own message gets through, so the hook stays quiet.
p2 = payload("ExitPlanMode", {"plan": "## Plan\n- Small change.\n" * 40})
plug2 = run(PLUGIN, p2)
check("short plan: Claude accepts the plugin message", claude_accepts(plug2), len(plug2.encode()))
check("short plan: hook prints nothing", run(SHIM, p2) is None)

# 3. The exact boundary: the hook speaks up exactly when the plugin message passes 4096 bytes.
def plan_of(n): return payload("ExitPlanMode", {"plan": "x" * n})
lo, hi = 1000, 6000
while lo < hi:
    mid = (lo + hi + 1) // 2
    if len(run(PLUGIN, plan_of(mid)).encode()) <= LIMIT: lo = mid
    else: hi = mid - 1
at, over = plan_of(lo), plan_of(lo + 1)
check("boundary: plugin message is exactly <= 4096 at n", len(run(PLUGIN, at).encode()) <= LIMIT and len(run(PLUGIN, over).encode()) > LIMIT)
check("boundary: hook quiet at the last size that fits", run(SHIM, at) is None)
check("boundary: hook speaks at the first size that doesn't", run(SHIM, over) is not None)

# 4. Multibyte text: the limit is bytes, not characters.
uni = payload("ExitPlanMode", {"plan": "Планировка для iPad — проверка " * 60})
plug4, shim4 = run(PLUGIN, uni), run(SHIM, uni)
check("multibyte: hook speaks iff the plugin message is over 4096 bytes",
      (len(plug4.encode()) > LIMIT) == (shim4 is not None), (len(plug4.encode()), shim4 is not None))
check("multibyte: short copy accepted and parsed", shim4 is None or (claude_accepts(shim4) and warposs_event(shim4) is not None))

# 4b. Content with escapes, quotes, tabs, emoji and semicolons: the hook's size check must agree
# with the plugin's real output at the exact boundary, for every kind of text.
def boundary_agrees(make):
    lo, hi = 50, 8000
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if len(run(PLUGIN, make(mid)).encode()) <= LIMIT: lo = mid
        else: hi = mid - 1
    fits, over = make(lo), make(lo + 1)
    return run(SHIM, fits) is None and len(run(PLUGIN, over).encode()) > LIMIT and run(SHIM, over) is not None
chunk = 'Line "quoted" \\ back\\slash\ttab; emoji \U0001F600; done\n'
chunk = chunk.encode().decode("unicode_escape").encode("latin-1").decode("utf-8")
def mixed(n): return payload("ExitPlanMode", {"plan": (chunk * 200)[:n], "planFilePath": "/Users/me/.claude/plans/ipad-duo-layout.md"})
def bash_cmd(n): return payload("Bash", {"command": ("cd ~/x && make test; " * 400)[:n], "description": "Run tests"})
check("escapes, quotes, emoji: hook agrees with the plugin at the boundary", boundary_agrees(mixed))
check("long Bash: hook agrees with the plugin at the boundary", boundary_agrees(bash_cmd))
shim4b = run(SHIM, mixed(6000)); ev4b = shim4b and warposs_event(shim4b)
check("escapes, quotes, emoji: short copy accepted and parsed",
      bool(ev4b) and claude_accepts(shim4b) and ev4b["tool_name"] == "ExitPlanMode", shim4b and len(shim4b.encode()))

# 5. A large Write keeps its file path.
w = payload("Write", {"file_path": "/Users/me/app/Layout.swift", "content": "let a = 1; " * 1000})
shim5 = run(SHIM, w); ev5 = shim5 and warposs_event(shim5)
check("large Write: short copy keeps file_path", bool(ev5) and ev5["tool_input"] == {"file_path": "/Users/me/app/Layout.swift"}, ev5)
check("large Write: summary names the file", bool(ev5) and ev5["summary"] == "Wants to run Write: /Users/me/app/Layout.swift", ev5)

# 6. A long Bash command full of semicolons: command cut to 200 chars, semicolons gone.
b = payload("Bash", {"command": "echo a; " * 600, "description": "loop"})
shim6 = run(SHIM, b); ev6 = shim6 and warposs_event(shim6)
check("long Bash: parsed, command <= 200 chars, no semicolons",
      bool(ev6) and len(ev6["tool_input"]["command"]) <= 200 and ";" not in ev6["tool_input"]["command"] and ";" not in ev6["summary"], ev6)

# 7. Quiet cases.
check("outside Warp: quiet", run(SHIM, p1, env={k: v for k, v in ENV.items() if not k.startswith("WARP_")}) is None)
check("old Claude Code (plugin writes /dev/tty itself): quiet", run(SHIM, p1, env={**ENV, "CLAUDE_CODE_VERSION": "2.1.100"}) is None)
check("broken Warp release: quiet", run(SHIM, p1, env={**ENV, "WARP_CLIENT_VERSION": "v0.2026.03.25.08.24.stable_05"}) is None)
check("garbage input: quiet, exit 0", run(SHIM, None, raw="not json") is None)
check("empty input: quiet, exit 0", run(SHIM, None, raw="") is None)
check("tool_input not an object: no crash", run(SHIM, payload("ExitPlanMode", "x" * 5000)) is None or True)

print("FAILURES:", failures)
sys.exit(1 if failures else 0)
