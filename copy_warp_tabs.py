#!/usr/bin/env python3
"""Copy official Warp's tabs and agent sessions into WarpOss.

Official Warp is only read, never changed: the script reads the process list,
Claude Code's ~/.claude/sessions/<pid>.json files, and a read-only backup of
Warp's database. It never signals, types into, or closes anything in official
Warp. Each agent tab in WarpOss opens a *copy* of its conversation under a new
session id (`claude --resume <id> --fork-session --session-id <new id>`,
`codex fork <id>`), so the original keeps running and its transcript is never
written by WarpOss. Each Claude copy names itself after the original with a
local /rename (no model call), which also saves its transcript right away, so
WarpOss can bring the copy back after a restart even before you use it.

    python3 copy_warp_tabs.py            # dry run: print and save the plan
    python3 copy_warp_tabs.py --apply    # write the launch config, open it in WarpOss

Options:
    --start enter    wait for Enter in each agent tab instead of opening copies on their own
    --agents-only    leave plain shell tabs out
    --again          make new copies of sessions that already have one
    --force          replace an existing "Copied from Warp" launch config
    --no-db          don't read official Warp's database (no titles, colors, splits)
"""

import argparse
import datetime as dt
import glob
import json
import os
import re
import shlex
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.parse
import uuid
from pathlib import Path

HOME = Path.home()
MIGRATE_DIR = HOME / "SideProjects" / "warp-custom" / "migrate"
HELPER = MIGRATE_DIR / "open-copy.sh"
SCRIPT_NAME = "copy_warp_tabs.py"
COPIES_FILE = MIGRATE_DIR / "copies.json"
CLAUDE_DIR = HOME / ".claude"
WARPOSS_APP = Path("/Applications/WarpOss.app")
WARPOSS_LAUNCH_DIR = HOME / ".warp-oss" / "launch_configurations"
CONFIG_NAME = "Copied from Warp"
CONFIG_FILE = "copied-from-warp.yaml"
OFFICIAL_WARP_MARKER = "/Warp.app/Contents/MacOS/"
ANSI_COLORS = ("black", "red", "green", "yellow", "blue", "magenta", "cyan", "white")
UUID_RE = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
BLOCK_SESSION_RE = re.compile(r"^precmd-(-?\d+)-\d+$")
# A word short and plain enough to show from a command line that's still running.
HINT_WORD_RE = re.compile(r"^[A-Za-z0-9.][A-Za-z0-9._:@+/-]{0,23}$")
START_MATCH_SECONDS = 5
# Copies open this far apart, so 19 sessions and their MCP servers don't all start at once.
STAGGER_SECONDS = 2

# Claude Code flags carried over to the copy (resume doesn't restore these). Other flags are
# listed in the plan.
CARRIED_FLAGS_WITH_VALUE = {"--effort", "--settings", "--agent", "--fallback-model"}
CARRIED_FLAGS_WITH_VALUES = {"--add-dir", "--mcp-config", "--plugin-dir"}
# Resume restores the model and permission mode the session was last in, so the launch values
# would undo a later /model or mode switch. Bypass mode isn't restored, so it is carried.
RESTORED_FLAGS_WITH_VALUE = {"--model", "--permission-mode"}
CARRIED_SWITCHES = {
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--chrome",
    "--no-chrome",
    "--strict-mcp-config",
}
# Flags that pick or name the session; the copy gets its own id.
SESSION_FLAGS_WITH_VALUE = {"-r", "--resume", "--session-id", "-n", "--name"}
SESSION_SWITCHES = {"-c", "--continue", "--fork-session"}
# Claude processes that aren't a conversation of their own.
CLAUDE_SUBCOMMANDS = {"agents", "attach"}


def run(args, cwd=None):
    env = dict(os.environ, LC_ALL="C")
    try:
        result = subprocess.run(args, capture_output=True, text=True, cwd=cwd, env=env)
    except OSError:
        return ""
    return result.stdout


# --- processes -------------------------------------------------------------------


class Proc:
    def __init__(self, pid, ppid, rss_kb, tty, start, comm):
        self.pid = pid
        self.ppid = ppid
        self.rss_kb = rss_kb
        self.tty = tty
        self.start = start
        self.comm = comm
        self.name = os.path.basename(comm)
        self.args = None


def read_processes():
    procs = {}
    for line in run(["ps", "-axo", "pid=,ppid=,rss=,tty=,lstart=,comm="]).splitlines():
        parts = line.split(None, 9)
        if len(parts) < 10:
            continue
        try:
            start = time.mktime(time.strptime(" ".join(parts[4:9]), "%a %b %d %H:%M:%S %Y"))
            procs[int(parts[0])] = Proc(int(parts[0]), int(parts[1]), int(parts[2]), parts[3], start, parts[9])
        except ValueError:
            continue
    return procs


def ancestors(procs, pid):
    seen = set()
    proc = procs.get(pid)
    while proc and proc.ppid in procs and proc.ppid not in seen:
        seen.add(proc.ppid)
        proc = procs[proc.ppid]
        yield proc


def descendants(procs, pid):
    children = {}
    for p in procs.values():
        children.setdefault(p.ppid, []).append(p.pid)
    stack, out = [pid], []
    while stack:
        for child in children.get(stack.pop(), []):
            out.append(child)
            stack.append(child)
    return out


def process_args(pid):
    """Full argv of one process. Only called for processes inside official Warp's panes."""
    return run(["ps", "-o", "args=", "-p", str(pid)]).strip()


def agent_kind(proc):
    if proc.name == "claude":
        return "claude"
    if proc.name == "codex":
        return "codex"
    if proc.name == "node":
        if proc.args is None:
            proc.args = process_args(proc.pid)
        words = proc.args.split()
        if len(words) > 1 and os.path.basename(words[1]) == "codex":
            return "codex"
    return None


def cwd_of(pids):
    cwds = {}
    if not pids:
        return cwds
    out = run(["lsof", "-a", "-d", "cwd", "-Fpn", "-p", ",".join(str(p) for p in pids)])
    pid = None
    for line in out.splitlines():
        if line.startswith("p"):
            pid = int(line[1:])
        elif line.startswith("n") and pid is not None:
            cwds[pid] = line[1:]
    return cwds


def codex_session_id(procs, pid):
    """The rollout file Codex (or its native child) keeps open."""
    pids = [pid] + descendants(procs, pid)
    out = run(["lsof", "-Fn", "-p", ",".join(str(p) for p in pids)])
    for line in out.splitlines():
        if line.startswith("n") and "/.codex/sessions/" in line and line.endswith(".jsonl"):
            ids = UUID_RE.findall(os.path.basename(line))
            if ids:
                return ids[-1]
    return None


def claude_session_record(pid):
    """Claude Code's record for a running process: sessionId, cwd, status (busy or idle), ..."""
    try:
        return json.loads((CLAUDE_DIR / "sessions" / f"{pid}.json").read_text())
    except (OSError, ValueError):
        return None


def find_key(record, *keys):
    if not isinstance(record, dict):
        return None
    for key in keys:
        value = record.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def claude_transcript(session_id):
    matches = glob.glob(str(CLAUDE_DIR / "projects" / "*" / f"{session_id}.jsonl"))
    return Path(matches[0]) if matches else None


def claude_transcript_facts(transcript):
    """The session's last custom title and last AI title, and the folders its messages ran in."""
    facts = {"custom": None, "ai": None, "cwds": []}
    try:
        with open(transcript, "rb") as f:
            for raw in f:
                has_cwd = b'"cwd"' in raw
                if not has_cwd and b'"custom-title"' not in raw and b'"ai-title"' not in raw:
                    continue
                try:
                    entry = json.loads(raw)
                except ValueError:
                    continue
                if not isinstance(entry, dict):
                    continue
                if entry.get("type") == "custom-title" and entry.get("customTitle"):
                    facts["custom"] = entry["customTitle"]
                elif entry.get("type") == "ai-title" and entry.get("aiTitle"):
                    facts["ai"] = entry["aiTitle"]
                cwd = entry.get("cwd") if has_cwd else None
                if isinstance(cwd, str) and cwd and cwd not in facts["cwds"]:
                    facts["cwds"].append(cwd)
    except OSError:
        pass
    return facts


def claude_project_dir_name(path):
    """The folder name Claude Code keeps a project's transcripts under."""
    return re.sub(r"[^a-zA-Z0-9]", "-", path)


def claude_resume_cwd(transcript, candidates):
    """The folder to start the copy in, so its tools work on the same project: the first folder
    whose project holds the transcript."""
    for candidate in candidates:
        path = norm(candidate)
        if path and claude_project_dir_name(path) == transcript.parent.name:
            return path
    return None


def split_claude_flags(args):
    """Splits the original command line into flags the copy keeps and flags it drops.

    `ps` joins argv with spaces, so values that contained spaces can't be told apart.
    """
    words = args.split()[1:]
    carried, dropped = [], []
    i = 0
    while i < len(words):
        word = words[i]
        flag, _, inline = word.partition("=")
        if flag in CARRIED_SWITCHES:
            carried.append(word)
        elif flag in CARRIED_FLAGS_WITH_VALUE:
            if inline:
                carried.append(word)
            elif i + 1 < len(words):
                carried += [word, words[i + 1]]
                i += 1
        elif flag in CARRIED_FLAGS_WITH_VALUES:
            carried.append(word)
            while i + 1 < len(words) and not words[i + 1].startswith("-"):
                carried.append(words[i + 1])
                i += 1
        elif flag in RESTORED_FLAGS_WITH_VALUE:
            value = inline
            if not inline and i + 1 < len(words):
                value = words[i + 1]
                i += 1
            if flag == "--permission-mode" and value == "bypassPermissions":
                carried += [flag, value]
        elif flag in SESSION_FLAGS_WITH_VALUE:
            if not inline and i + 1 < len(words) and not words[i + 1].startswith("-"):
                i += 1
        elif flag not in SESSION_SWITCHES:
            dropped.append(word)
        i += 1
    return carried, dropped


def claude_subcommand(args):
    """["agents"] or ["attach", <id>] when the process is Claude's agent view or a terminal
    attached to a background session, else None."""
    words = args.split()[1:]
    value_flags = CARRIED_FLAGS_WITH_VALUE | RESTORED_FLAGS_WITH_VALUE | SESSION_FLAGS_WITH_VALUE
    i = 0
    while i < len(words):
        word = words[i]
        if word.startswith("-"):
            flag, _, inline = word.partition("=")
            if flag in value_flags and not inline:
                i += 1
            i += 1
            continue
        return [word] + words[i + 1:i + 2] if word in CLAUDE_SUBCOMMANDS else None
    return None


def official_warp_pids(procs):
    return {pid for pid, p in procs.items() if OFFICIAL_WARP_MARKER in p.comm}


def find_agents(procs, warp):
    """Agents running directly in official Warp's panes, not ones started by another agent."""
    agents = []
    for proc in procs.values():
        chain = list(ancestors(procs, proc.pid))
        if not any(a.pid in warp for a in chain):
            continue
        kind = agent_kind(proc)
        if not kind:
            continue
        below_warp = []
        for a in chain:
            if a.pid in warp:
                break
            below_warp.append(a)
        if any(agent_kind(a) for a in below_warp):
            continue
        shell = below_warp[-1] if below_warp else None
        agents.append({"proc": proc, "kind": kind, "shell": shell})
    return agents


def pane_roots(procs, warp):
    return [p for p in procs.values() if p.ppid in warp and p.pid not in warp and p.tty not in ("??", "-")]


def memory_report(procs, agents):
    """What the agent sessions use now (with their MCP servers and tools) and what macOS reports
    as available, in GB."""
    used_kb = 0
    for agent in agents:
        pid = agent["proc"].pid
        used_kb += sum(procs[p].rss_kb for p in [pid] + descendants(procs, pid) if p in procs)
    available = None
    out = run(["vm_stat"])
    page = re.search(r"page size of (\d+) bytes", out)
    if page:
        pages = 0
        for label in ("Pages free", "Pages inactive", "Pages speculative", "Pages purgeable"):
            found = re.search(rf"^{label}:\s+(\d+)", out, re.M)
            pages += int(found.group(1)) if found else 0
        available = pages * int(page.group(1)) / 1e9
    total = run(["sysctl", "-n", "hw.memsize"]).strip()
    return {
        "agents_gb": round(used_kb / 1e6, 1),
        "available_gb": round(available, 1) if available is not None else None,
        "total_gb": round(int(total) / 1e9) if total.isdigit() else None,
    }


# --- official Warp's database ----------------------------------------------------


def find_warp_db():
    candidates = glob.glob(str(HOME / "Library" / "Group Containers" / "*.dev.warp" / "Library"
                               / "Application Support" / "dev.warp.Warp-Stable" / "warp.sqlite"))
    candidates.append(str(HOME / "Library" / "Application Support" / "dev.warp.Warp-Stable" / "warp.sqlite"))
    existing = [c for c in candidates if os.path.isfile(c)]

    def last_write(path):
        return max(os.path.getmtime(p) for p in (path, path + "-wal") if os.path.exists(p))

    return max(existing, key=last_write) if existing else None


def snapshot_warp_db(path, dest):
    """Copies the live database through a read-only connection (SQLite's backup API)."""
    src = sqlite3.connect("file:" + urllib.parse.quote(path) + "?mode=ro", uri=True)
    try:
        dst = sqlite3.connect(dest)
        try:
            src.backup(dst)
        finally:
            dst.close()
    finally:
        src.close()


def select(conn, table, wanted, extra=""):
    have = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
    cols = [c for c in wanted if c in have]
    if not cols:
        return []
    rows = conn.execute(f"SELECT {', '.join(cols)} FROM {table} {extra}").fetchall()
    return [dict(zip(cols, row)) for row in rows]


def uuid_key(value):
    if value is None:
        return None
    return bytes(value) if isinstance(value, (bytes, bytearray, memoryview)) else str(value).encode()


def parse_color(value):
    if not value:
        return None
    lowered = str(value).lower()
    for color in ANSI_COLORS:
        if re.search(rf"\b{color}\b", lowered):
            return color
    return None


def parse_db_time(value):
    """Both readings of a stored timestamp (UTC and local), in epoch seconds."""
    if not value:
        return None
    text = str(value).replace("T", " ").split("+")[0].rstrip("Z")
    for fmt in ("%Y-%m-%d %H:%M:%S.%f", "%Y-%m-%d %H:%M:%S"):
        try:
            naive = dt.datetime.strptime(text, fmt)
            break
        except ValueError:
            continue
    else:
        return None
    return {
        "utc": naive.replace(tzinfo=dt.timezone.utc).timestamp(),
        "local": time.mktime(naive.timetuple()) + naive.microsecond / 1e6,
    }


def read_layout(snapshot):
    conn = sqlite3.connect(snapshot)
    try:
        windows = select(conn, "windows", ["id", "active_tab_index", "quake_mode"], "ORDER BY id")
        app = select(conn, "app", ["active_window_id"])
        tabs = select(conn, "tabs", ["id", "window_id", "custom_title", "color", "tab_group_id"], "ORDER BY id")
        groups = select(conn, "tab_groups", ["id", "window_id", "name", "color", "collapsed", "pinned"], "ORDER BY id")
        nodes = select(conn, "pane_nodes", ["id", "tab_id", "parent_pane_node_id", "is_leaf"], "ORDER BY id")
        branches = {r["pane_node_id"]: r for r in select(conn, "pane_branches", ["pane_node_id", "horizontal"])}
        leaves = {r["pane_node_id"]: r for r in select(conn, "pane_leaves", ["pane_node_id", "kind", "is_focused"])}
        terminals = {r["id"]: r for r in select(conn, "terminal_panes", ["id", "uuid", "cwd"])}
        block_sessions = {}
        for pane_uuid, block_id in conn.execute("SELECT pane_leaf_uuid, block_id FROM blocks"):
            match = BLOCK_SESSION_RE.match(block_id or "")
            if match and pane_uuid is not None:
                block_sessions.setdefault(uuid_key(pane_uuid), set()).add(int(match.group(1)))
        running = select(conn, "commands", ["command", "start_ts", "pwd", "session_id"],
                         "WHERE completed_ts IS NULL AND start_ts IS NOT NULL")
    finally:
        conn.close()

    children = {}
    for node in nodes:
        children.setdefault(node["parent_pane_node_id"], []).append(node)

    def build(node):
        if node["is_leaf"]:
            leaf = leaves.get(node["id"], {})
            terminal = terminals.get(node["id"])
            if leaf.get("kind") != "terminal" or terminal is None:
                return None
            sessions = block_sessions.get(uuid_key(terminal.get("uuid")), set())
            # The pane this script runs in isn't copied.
            if any(r.get("session_id") in sessions and SCRIPT_NAME in (r.get("command") or "") for r in running):
                return None
            # A non-agent command still running here, such as a dev server.
            busy = [r["command"] for r in running
                    if r.get("session_id") in sessions and r.get("command") and not runs_agent(r["command"])]
            return {
                "type": "pane",
                "cwd": terminal.get("cwd"),
                "is_focused": bool(leaf.get("is_focused")),
                "shell_sessions": sessions,
                "running": busy[-1] if busy else None,
            }
        built = [b for b in (build(c) for c in children.get(node["id"], [])) if b]
        if not built:
            return None
        if len(built) == 1:
            return built[0]
        horizontal = bool(branches.get(node["id"], {}).get("horizontal"))
        return {"type": "split", "direction": "horizontal" if horizontal else "vertical", "panes": built}

    roots = {n["tab_id"]: n for n in nodes if n["parent_pane_node_id"] is None}
    active_window_id = app[0]["active_window_id"] if app else None
    layout = {"windows": [], "running": running}
    for window in windows:
        if window.get("quake_mode"):
            continue
        window_groups = [g for g in groups if g["window_id"] == window["id"]]
        group_ids = [g["id"] for g in window_groups]
        # Warp saves a window's tabs in their on-screen order, and active_tab_index counts them.
        window_tabs = [t for t in tabs if t["window_id"] == window["id"]]
        active_index = window.get("active_tab_index")
        active_tab_id = (window_tabs[active_index]["id"]
                         if isinstance(active_index, int) and 0 <= active_index < len(window_tabs) else None)
        out_tabs = []
        for tab in window_tabs:
            if tab["id"] not in roots:
                continue
            root = build(roots[tab["id"]])
            if root is None:
                continue
            group_id = tab.get("tab_group_id")
            out_tabs.append({
                "title": tab.get("custom_title"),
                "color": parse_color(tab.get("color")),
                "group": group_ids.index(group_id) if group_id in group_ids else None,
                "active": tab["id"] == active_tab_id,
                "root": root,
            })
        layout["windows"].append({
            "active": window["id"] == active_window_id,
            "groups": [{"name": g.get("name"), "color": parse_color(g.get("color")),
                        "collapsed": bool(g.get("collapsed")), "pinned": bool(g.get("pinned"))}
                       for g in window_groups],
            "tabs": out_tabs,
        })
    return layout


def panes_of(node):
    if node["type"] == "pane":
        yield node
    else:
        for child in node["panes"]:
            yield from panes_of(child)


# --- putting agents into panes ---------------------------------------------------


def norm(path):
    return os.path.normpath(os.path.expanduser(path)) if path else None


def runs_agent(command):
    """Whether a command line starts claude or codex, e.g. `claude`, `cd x && claude -c`."""
    words = re.split(r"[\s;&|()]+", command or "")
    return any(os.path.basename(w) in ("claude", "codex") for w in words)


def in_folder(pane, agent):
    return norm(pane["cwd"]) in {norm(agent["cwd"]), agent.get("resume_cwd")} - {None}


def place_agents(agents, layout):
    """Puts each agent into a pane of the layout. Returns the agents nobody could place."""
    panes = [p for w in layout["windows"] for t in w["tabs"] for p in panes_of(t["root"])]
    taken = set()

    def take(pane, agent, how):
        pane["agent"] = agent
        pane["running"] = None
        agent["placed_by"] = how
        taken.add(id(pane))

    # 1. The agent's start time matches a running command, and that command's shell session
    #    shows up in exactly one pane's blocks. Both readings of the stored time are tried.
    running = [r for r in layout["running"] if runs_agent(r.get("command"))]
    best = {}
    for reading in ("utc", "local"):
        found = {}
        for agent in agents:
            near = []
            for r in running:
                times = parse_db_time(r.get("start_ts"))
                if times and abs(times[reading] - agent["proc"].start) <= START_MATCH_SECONDS:
                    near.append((abs(times[reading] - agent["proc"].start), r))
            if near:
                found[agent["proc"].pid] = min(near, key=lambda x: x[0])[1]
        if len(found) > len(best):
            best = found
    for agent in agents:
        command = best.get(agent["proc"].pid)
        if command:
            hits = [p for p in panes if id(p) not in taken and command.get("session_id") in p["shell_sessions"]]
            if len(hits) == 1:
                take(hits[0], agent, "shell session")

    # 2. Only one free pane in the agent's folder.
    for agent in agents:
        if not agent.get("placed_by"):
            hits = [p for p in panes if id(p) not in taken and in_folder(p, agent)]
            if len(hits) == 1:
                take(hits[0], agent, "folder")

    # 3. Several free panes in the folder: oldest session first, left to right. Swapping two
    #    sessions of one project only changes which of those tabs each lands in.
    for agent in sorted(agents, key=lambda a: a["proc"].start):
        if not agent.get("placed_by"):
            hits = [p for p in panes if id(p) not in taken and in_folder(p, agent)]
            if hits:
                take(hits[0], agent, "folder, order guessed")

    return [a for a in agents if not a.get("placed_by")]


def layout_from_processes(procs, warp, agents):
    """Without the database: one tab per pane in official Warp, oldest first, no splits."""
    mine = {p.pid for p in ancestors(procs, os.getpid())}
    roots = sorted((p for p in pane_roots(procs, warp) if p.pid not in mine), key=lambda p: p.start)
    cwds = cwd_of([p.pid for p in roots])
    by_shell = {a["shell"].pid: a for a in agents if a["shell"]}
    tabs = []
    for root in roots:
        pane = {"type": "pane", "cwd": cwds.get(root.pid), "is_focused": True, "shell_sessions": set(),
                "running": None}
        agent = by_shell.get(root.pid)
        if agent:
            pane["agent"] = agent
            agent["placed_by"] = "process"
        tabs.append({"title": None, "color": None, "group": None, "active": False, "root": pane})
    return {"windows": [{"active": True, "groups": [], "tabs": tabs}], "running": []}


# --- the WarpOss launch configuration ----------------------------------------------


def clean_name(text, fallback):
    text = re.sub(r"[\x00-\x1f\x7f]+", " ", text or "").strip()
    return (text or fallback)[:100]


def copy_name(agent):
    return clean_name(agent.get("name"), os.path.basename(norm(agent.get("resume_cwd") or agent["cwd"]) or "") or "copy")


def command_hint(command):
    """The first plain words of a command, quoted, such as "npm run dev". Values, flags and
    anything long are left out, since a command line can carry a key."""
    words = (command or "").split()
    while words and "=" in words[0]:
        words.pop(0)
    hint = []
    for word in words:
        if len(hint) == 4 or not HINT_WORD_RE.match(word):
            break
        hint.append(word)
    if not hint:
        return "a command"
    return '"' + " ".join(hint) + (" ..." if len(hint) < len(words) else "") + '"'


def quoted(words):
    return " ".join(shlex.quote(w) for w in words)


def helper_command(mode, delay, agent):
    return quoted(["/bin/bash", str(HELPER), mode, str(delay), str(agent["proc"].pid), copy_name(agent)])


def pane_commands(pane, start, delays):
    agent = pane.get("agent")
    if not agent:
        if pane.get("running"):
            note = ("In official Warp, this tab is still running " + command_hint(pane["running"])
                    + ". Start it here once you stop it there.")
            return ["echo " + shlex.quote(note)]
        return []
    subcommand = agent.get("subcommand")
    if subcommand and subcommand[0] == "agents":
        return ["claude agents"]
    if subcommand and subcommand[0] == "attach" and len(subcommand) > 1:
        return [helper_command("attach", 0, agent), quoted(["claude", "attach", subcommand[1]])]
    if subcommand:
        return ["claude agents"]
    if not agent.get("session_id"):
        if agent["kind"] == "claude":
            note = ("Official Warp ran a Claude Code session here, but its id could not be read. "
                    "To copy it, run: claude --resume --fork-session (then pick it from the list)")
        else:
            note = ("Official Warp ran a Codex session here, but its id could not be read. "
                    "To copy it, run: codex fork (then pick it from the list)")
        return ["echo " + shlex.quote(note)]
    if agent["kind"] == "claude" and not agent.get("transcript"):
        # Nothing was said in it yet, so a new session is the same thing.
        return [quoted(["claude"] + agent["carried"])]
    commands = [helper_command(start, delays.get(id(agent), 0), agent)]
    if agent["kind"] == "claude":
        # The prompt goes before the carried flags: --add-dir and friends take several values.
        commands.append(quoted(["claude", "--resume", agent["session_id"], "--fork-session",
                                "--session-id", agent["copy_id"], "/rename " + copy_name(agent)]
                               + agent["carried"]))
    else:
        commands.append(quoted(["codex", "fork", agent["session_id"]]))
    return commands


def y(value):
    return json.dumps(value, ensure_ascii=False)


def emit_node(node, indent, start, delays, lines, first_prefix):
    pad = " " * indent
    if node["type"] == "split":
        lines.append(f"{first_prefix}split_direction: {node['direction']}")
        lines.append(f"{pad}panes:")
        for child in node["panes"]:
            emit_node(child, indent + 4, start, delays, lines, " " * (indent + 2) + "- ")
        return
    agent = node.get("agent") or {}
    lines.append(f"{first_prefix}cwd: {y(agent.get('resume_cwd') or node['cwd'] or str(HOME))}")
    if node.get("is_focused"):
        lines.append(f"{pad}is_focused: true")
    commands = pane_commands(node, start, delays)
    if commands:
        lines.append(f"{pad}commands:")
        for command in commands:
            lines.append(f"{pad}  - exec: {y(command)}")


def prune(node, keep):
    if node["type"] == "pane":
        return node if keep(node) else None
    kept = [c for c in (prune(child, keep) for child in node["panes"]) if c]
    if not kept:
        return None
    return kept[0] if len(kept) == 1 else dict(node, panes=kept)


def launch_config(layout, start, keep):
    lines = [
        f"# Made by copy_warp_tabs.py on {dt.datetime.now():%Y-%m-%d %H:%M}.",
        "# Agent tabs open a copy of their conversation; official Warp is untouched.",
        f"name: {y(CONFIG_NAME)}",
    ]
    windows = []
    for window in layout["windows"]:
        tabs = []
        for tab in window["tabs"]:
            root = prune(tab["root"], keep)
            if root:
                tabs.append(dict(tab, root=root))
        if tabs:
            windows.append(dict(window, tabs=tabs))
    # Copies that open on their own start STAGGER_SECONDS apart, in tab order.
    delays = {}
    for window in windows:
        for tab in window["tabs"]:
            for pane in panes_of(tab["root"]):
                agent = pane.get("agent")
                if agent and agent.get("session_id") and agent.get("transcript") and not agent.get("subcommand"):
                    delays[id(agent)] = len(delays) * STAGGER_SECONDS
    active_window = next((i for i, w in enumerate(windows) if w["active"]), None)
    if active_window is not None:
        lines.append(f"active_window_index: {active_window}")
    lines.append("windows:")
    for window in windows:
        tabs = window["tabs"]
        # Keep only groups that still have a tab, renumbered.
        used = sorted({t["group"] for t in tabs if t["group"] is not None})
        renumber = {old: new for new, old in enumerate(used)}
        active_tab = next((i for i, t in enumerate(tabs) if t.get("active")), None)
        first = "  - "
        if active_tab is not None:
            lines.append(f"{first}active_tab_index: {active_tab}")
            first = "    "
        lines.append(f"{first}tabs:")
        for tab in tabs:
            first = "      - "
            if tab["title"]:
                lines.append(f"{first}title: {y(tab['title'])}")
                first = "        "
            if tab["color"]:
                lines.append(f"{first}color: {tab['color']}")
                first = "        "
            if tab["group"] is not None:
                lines.append(f"{first}group: {renumber[tab['group']]}")
                first = "        "
            lines.append(f"{first}layout:")
            emit_node(tab["root"], 10, start, delays, lines, " " * 10)
        if used:
            lines.append("    tab_groups:")
            for old in used:
                group = window["groups"][old]
                lines.append(f"      - name: {y(group['name'])}")
                if group["color"]:
                    lines.append(f"        color: {group['color']}")
                if group["collapsed"]:
                    lines.append("        collapsed: true")
                if group["pinned"]:
                    lines.append("        pinned: true")
    return "\n".join(lines) + "\n"


# --- main ---------------------------------------------------------------------------


def load_copies():
    try:
        copies = json.loads(COPIES_FILE.read_text())
        return copies if isinstance(copies, dict) else {}
    except (OSError, ValueError):
        return {}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--agents-only", action="store_true")
    parser.add_argument("--start", choices=("auto", "enter"), default="auto")
    parser.add_argument("--again", action="store_true")
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--no-db", action="store_true")
    opts = parser.parse_args()

    MIGRATE_DIR.mkdir(parents=True, exist_ok=True)
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    warnings = []
    copies = load_copies()

    procs = read_processes()
    warp = official_warp_pids(procs)
    if not warp:
        sys.exit("Official Warp isn't running, so there is nothing to copy.")
    agents = find_agents(procs, warp)

    cwds = cwd_of(sorted({a["proc"].pid for a in agents} | {a["shell"].pid for a in agents if a["shell"]}))
    for agent in agents:
        proc = agent["proc"]
        proc.args = proc.args or process_args(proc.pid)
        agent["cwd"] = cwds.get(agent["shell"].pid if agent["shell"] else proc.pid) or cwds.get(proc.pid)
        agent.update(resume_cwd=None, name=None, last_write=None, transcript=None, status=None,
                     subcommand=None, copy_id=None, copied=None, carried=[], dropped=[])
        if agent["kind"] == "claude":
            record = claude_session_record(proc.pid)
            agent["subcommand"] = claude_subcommand(proc.args)
            agent["status"] = find_key(record, "status")
            agent["session_id"] = None if agent["subcommand"] else find_key(record, "sessionId", "session_id")
            agent["carried"], agent["dropped"] = split_claude_flags(proc.args)
            transcript = claude_transcript(agent["session_id"]) if agent["session_id"] else None
            if transcript:
                facts = claude_transcript_facts(transcript)
                agent["transcript"] = str(transcript)
                agent["name"] = facts["custom"] or facts["ai"]
                agent["last_write"] = os.path.getmtime(transcript)
                agent["resume_cwd"] = claude_resume_cwd(
                    transcript, [cwds.get(proc.pid), find_key(record, "cwd"), agent["cwd"]] + facts["cwds"])
                if not agent["resume_cwd"]:
                    warnings.append(f"pid {proc.pid}: couldn't tell which folder session {agent['session_id']} "
                                    f"belongs to ({transcript.parent.name}); its tab starts in {agent['cwd']}")
                # A copy made earlier counts once Claude saved it (it saves itself when it opens).
                earlier = copies.get(agent["session_id"]) or {}
                if earlier.get("copy_id") and claude_transcript(earlier["copy_id"]):
                    agent["copied"] = earlier.get("made") or "earlier"
                    agent["copy_id"] = str(uuid.uuid4()) if opts.again else earlier["copy_id"]
                else:
                    agent["copy_id"] = earlier.get("copy_id") or str(uuid.uuid4())
        else:
            agent["session_id"] = codex_session_id(procs, proc.pid)
            agent["transcript"] = agent["session_id"]
        if not agent["session_id"] and not agent["subcommand"]:
            warnings.append(f"pid {proc.pid}: couldn't read its {agent['kind']} session id")

    layout = None
    if not opts.no_db:
        db = find_warp_db()
        if db:
            snapshot = MIGRATE_DIR / "warp-stable-snapshot.sqlite"
            try:
                snapshot_warp_db(db, str(snapshot))
                layout = read_layout(str(snapshot))
            except (sqlite3.Error, OSError) as err:
                warnings.append(f"couldn't read official Warp's database ({err}), so tabs follow the process list")
        else:
            warnings.append("official Warp's database wasn't found, so tabs follow the process list")
    if layout:
        for agent in place_agents(agents, layout):
            agent["placed_by"] = "new tab at the end"
            target = next((w for w in layout["windows"] if w["active"]), layout["windows"][0])
            target["tabs"].append({"title": None, "color": None, "group": None, "active": False,
                                   "root": {"type": "pane", "cwd": agent["cwd"], "is_focused": True,
                                            "shell_sessions": set(), "running": None, "agent": agent}})
    else:
        layout = layout_from_processes(procs, warp, agents)

    reapply = any(a["copied"] for a in agents) and not opts.again

    def keep(pane):
        agent = pane.get("agent")
        if agent is None:
            return not (opts.agents_only or reapply)
        return not (reapply and (agent["copied"] or not agent["session_id"]))

    rows = []
    for wi, window in enumerate(layout["windows"]):
        for ti, tab in enumerate(window["tabs"]):
            for pane in panes_of(tab["root"]):
                agent = pane.get("agent") or {}
                rows.append({
                    "window": wi + 1, "tab": ti + 1, "folder": agent.get("resume_cwd") or pane["cwd"],
                    "tab_title": tab["title"], "color": tab["color"],
                    "agent": agent.get("kind"),
                    "pid": agent["proc"].pid if agent else None,
                    "session_id": agent.get("session_id"),
                    "copy_id": agent.get("copy_id"),
                    "name": agent.get("name"),
                    "status": agent.get("status"),
                    "subcommand": agent.get("subcommand"),
                    "placed_by": agent.get("placed_by"),
                    "kept_flags": agent.get("carried"),
                    "dropped_flags": agent.get("dropped"),
                    "last_write": (dt.datetime.fromtimestamp(agent["last_write"]).strftime("%m-%d %H:%M")
                                   if agent.get("last_write") else None),
                    "already_copied": agent.get("copied"),
                    "shell_still_running_something": bool(pane.get("running")),
                    "included": keep(pane),
                })
    yaml_text = launch_config(layout, opts.start, keep)
    memory = memory_report(procs, agents)
    plan_json = MIGRATE_DIR / f"plan-{stamp}.json"
    plan_yaml = MIGRATE_DIR / f"plan-{stamp}.yaml"
    plan_json.write_text(json.dumps({"made": stamp, "start": opts.start, "memory": memory, "rows": rows,
                                     "warnings": warnings}, indent=2, ensure_ascii=False))
    plan_yaml.write_text(yaml_text)

    tab_count = sum(len(w["tabs"]) for w in layout["windows"])
    print(f"\nOfficial Warp: {len(layout['windows'])} window(s), {tab_count} tabs, {len(agents)} agent sessions.\n")
    print(f"{'tab':<6}{'folder':<20}{'session':<40}{'status':<8}{'placed by':<23}included")
    for r in rows:
        folder = os.path.basename(norm(r["folder"]) or "") or "~"
        if r["agent"]:
            session = r["name"] or (" ".join(r["subcommand"]) if r["subcommand"] else None) or r["session_id"] or "id unknown"
        else:
            session = "(shell, still running something)" if r["shell_still_running_something"] else "(shell)"
        print(f"{r['window']}.{r['tab']:<4}{folder[:18]:<20}{session[:38]:<40}{(r['status'] or '')[:7]:<8}"
              f"{r['placed_by'] or '':<23}{'yes' if r['included'] else 'no'}")
    for w in warnings:
        print("  ! " + w)
    print(f"\nMemory: the agent sessions use about {memory['agents_gb']} GB with their MCP servers; "
          f"their copies will need about as much again. Available now: {memory['available_gb']} GB "
          f"of {memory['total_gb']} GB.")
    if reapply:
        print("Some sessions already have a copy, so only the others are included (--again makes new ones).")
    print(f"\nPlan: {plan_json}\nLaunch config preview: {plan_yaml}")

    if not opts.apply:
        print("Dry run: nothing was written outside the migrate folder. Add --apply to open the tabs in WarpOss.")
        return
    if not WARPOSS_APP.exists():
        sys.exit("WarpOss isn't installed at /Applications/WarpOss.app.")
    if not HELPER.exists():
        sys.exit(f"Missing {HELPER}.")
    target = WARPOSS_LAUNCH_DIR / CONFIG_FILE
    if target.exists() and not opts.force:
        sys.exit(f"{target} already exists. Add --force to replace it.")
    WARPOSS_LAUNCH_DIR.mkdir(parents=True, exist_ok=True)
    target.write_text(yaml_text)
    shutil.copy(plan_json, MIGRATE_DIR / f"applied-{stamp}.json")
    made = dt.datetime.now().strftime("%Y-%m-%d %H:%M")
    for agent in agents:
        if agent.get("copy_id") and agent.get("placed_by") and (opts.again or not agent["copied"]):
            copies[agent["session_id"]] = {"copy_id": agent["copy_id"], "made": made,
                                           "name": agent.get("name"), "folder": agent.get("resume_cwd")}
    COPIES_FILE.write_text(json.dumps(copies, indent=2, ensure_ascii=False))
    subprocess.run(["open", "warposs://launch/" + urllib.parse.quote(CONFIG_NAME)])
    print(f"\nOpened '{CONFIG_NAME}' in WarpOss. To undo, close that window and delete {target}.")


if __name__ == "__main__":
    main()
