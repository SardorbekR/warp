#!/bin/bash
# Runs in a WarpOss tab made by copy_warp_tabs.py, just before the tab opens its copy of a
# conversation that is still running in official Warp.
#
#   open-copy.sh <auto|enter|attach> <delay in seconds> <original pid> <name>
#
# Exits 0 when the copy should open, so WarpOss runs the next queued command (the copy).
# Ctrl-C exits non-zero, which skips it. Nothing here touches official Warp or the original.

mode=$1
delay=$2
pid=$3
name=$4
record="$HOME/.claude/sessions/$pid.json"

# Tab title while the tab waits.
printf '\033]0;%s\007' "$name"

# Whether the original is in the middle of a turn. Claude Code keeps "status" in its record.
original_busy() {
    kill -0 "$pid" 2>/dev/null || return 1
    grep -Eq '"status"[[:space:]]*:[[:space:]]*"busy"' "$record" 2>/dev/null
}

case $mode in
    enter | attach)
        printf '\n  %s\n\n' "$name"
        if [ "$mode" = attach ]; then
            printf '  Official Warp has this tab attached to a background session.\n'
            printf '  Press Enter to attach here too, or Ctrl-C to skip.\n\n'
        else
            printf '  Press Enter to open a copy of this conversation here. The original keeps\n'
            printf '  running in official Warp, untouched. Ctrl-C skips it.\n\n'
        fi
        read -r _ || exit 1
        exit 0
        ;;
esac

# Copies start a couple of seconds apart, so they don't all load at once.
if [ "$delay" -gt 0 ] 2>/dev/null; then
    sleep "$delay"
fi

if original_busy; then
    printf '\n  %s\n\n' "$name"
    printf '  The original is still working in official Warp. This tab opens its copy as soon as\n'
    printf '  it finishes, so the copy has the finished work. Enter opens it now; Ctrl-C skips it.\n\n'
    while original_busy; do
        # A timeout and Ctrl-D both return non-zero (macOS's bash 3.2 doesn't tell them apart),
        # so only a line read counts as Enter.
        if read -r -t 2 _; then
            break
        fi
        # Not a terminal, so read returns at once: wait here instead of spinning.
        [ -t 0 ] || sleep 2
    done
fi
exit 0
