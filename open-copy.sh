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

# Whether the original is still running and in the given state. Claude Code keeps "status" in
# its record: busy during a turn, waiting while it asks you something, idle after.
original_is() {
    kill -0 "$pid" 2>/dev/null || return 1
    grep -Eq "\"status\"[[:space:]]*:[[:space:]]*\"$1\"" "$record" 2>/dev/null
}

original_unfinished() {
    original_is busy || original_is waiting
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

if original_unfinished; then
    printf '\n  %s\n\n' "$name"
    if original_is waiting; then
        printf '  The original is waiting for your answer in official Warp. Answer it there, and this\n'
        printf '  tab opens its copy once that turn is done. Enter opens it now; Ctrl-C skips it.\n\n'
    else
        printf '  The original is still working in official Warp. This tab opens its copy as soon as\n'
        printf '  it finishes, so the copy has the finished work. Enter opens it now; Ctrl-C skips it.\n\n'
    fi
    while original_unfinished; do
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
