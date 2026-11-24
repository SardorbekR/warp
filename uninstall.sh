#!/bin/bash
# Removes the WarpOss "needs you" fallback hook: its entry in ~/.claude/settings.json and the script.
# Every other setting and hook stays as it is. A copy of settings.json is saved first.
set -euo pipefail

CLAUDE_DIR="${WARPOSS_CLAUDE_DIR:-$HOME/.claude}"
BACKUP_DIR="${WARPOSS_BACKUP_DIR:-$HOME/SideProjects/warp-custom/backups}"
SETTINGS="$CLAUDE_DIR/settings.json"
HOOK_NAME="warposs-permission-fallback.sh"

command -v jq >/dev/null 2>&1 || { echo "Stopped: jq is not installed." >&2; exit 1; }

if [ -e "$SETTINGS" ]; then
    jq -e 'type == "object"' "$SETTINGS" >/dev/null 2>&1 || { echo "Stopped: $SETTINGS is not valid JSON, nothing changed." >&2; exit 1; }
    FOUND=$(jq --arg name "$HOOK_NAME" '[(.hooks.PermissionRequest // [])[] | select(any(.hooks[]?; (.command? // "") | contains($name)))] | length' "$SETTINGS")
    if [ "$FOUND" != "0" ]; then
        mkdir -p "$BACKUP_DIR"
        STAMP=$(date -u +%Y%m%d-%H%M%S)
        BACKUP="$BACKUP_DIR/claude-settings-before-hook-uninstall-$STAMP.json"
        [ -e "$BACKUP" ] && BACKUP="${BACKUP%.json}-$$.json"
        cp -p "$SETTINGS" "$BACKUP"
        echo "Backup: $BACKUP"
        TMP="$SETTINGS.warposs-tmp"
        jq --arg name "$HOOK_NAME" '
            .hooks.PermissionRequest = [ .hooks.PermissionRequest[]
                | select(any(.hooks[]?; (.command? // "") | contains($name)) | not) ]
            | if (.hooks.PermissionRequest | length) == 0 then del(.hooks.PermissionRequest) else . end
            | if .hooks == {} then del(.hooks) else . end' "$SETTINGS" > "$TMP"
        if [ -L "$SETTINGS" ]; then
            cat "$TMP" > "$SETTINGS" && rm -f "$TMP"
        else
            MODE=$(stat -c %a "$SETTINGS" 2>/dev/null || stat -f %Lp "$SETTINGS")
            chmod "$MODE" "$TMP"
            mv "$TMP" "$SETTINGS"
        fi
        echo "Removed the hook entry from $SETTINGS"
    else
        echo "No hook entry in $SETTINGS"
    fi
fi
rm -f "$CLAUDE_DIR/hooks/$HOOK_NAME"
echo "Done. Open Claude sessions drop the hook on their own."
