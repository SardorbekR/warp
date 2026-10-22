#!/bin/sh
# Shown in a WarpOss tab made by copy_warp_tabs.py, before the tab opens its
# copy of a conversation that is still running in official Warp.
#
#   open-copy.sh <agent> <session-id> <name>
#
# Exits 0 when you press Enter, so WarpOss runs the next queued command (the
# fork). Ctrl-C exits non-zero, which cancels it. Nothing here touches official
# Warp or the original session.

case $1 in
    claude) agent="Claude Code" ;;
    codex) agent="Codex" ;;
    *) agent=$1 ;;
esac
id=$2
name=$3
marker_dir="$HOME/SideProjects/warp-custom/migrate/opened"
marker="$marker_dir/$id"

# Tab title while the tab waits.
printf '\033]0;%s\007' "$name"

printf '\n  %s\n\n' "$name"
printf '  This tab holds a copy of a %s conversation from official Warp\n' "$agent"
printf '  (session %s).\n\n' "$id"
if [ -e "$marker" ]; then
    printf '  You already opened a copy of it on %s.\n' "$(cat "$marker" 2>/dev/null)"
    printf '  Press Enter to make another copy anyway, or Ctrl-C to skip.\n\n'
else
    printf '  Press Enter to open the copy here. The original keeps running in\n'
    printf '  official Warp, untouched. Ctrl-C skips it.\n\n'
fi

read -r _ || exit 1

mkdir -p "$marker_dir" && date '+%Y-%m-%d %H:%M' > "$marker"
exit 0
