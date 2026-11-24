#!/bin/bash
# Installs the WarpOss "needs you" fallback hook for Claude Code (see warposs-permission-fallback.sh).
#
#   bash install.sh      install, or update an earlier install (safe to run again)
#
# What it does:
#   1. Tests the hook next to Warp's installed plugin. If a check fails, it stops and changes nothing.
#   2. Saves a copy of ~/.claude/settings.json in ~/SideProjects/warp-custom/backups.
#   3. Copies the hook to ~/.claude/hooks/ and adds one PermissionRequest entry to settings.json,
#      after checking that every other setting and hook stays exactly as it is. Open Claude sessions pick it up without a restart.
# Undo: bash uninstall.sh
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLAUDE_DIR="${WARPOSS_CLAUDE_DIR:-$HOME/.claude}"
BACKUP_DIR="${WARPOSS_BACKUP_DIR:-$HOME/SideProjects/warp-custom/backups}"
SETTINGS="$CLAUDE_DIR/settings.json"
HOOK_NAME="warposs-permission-fallback.sh"
HOOK_SRC="$SRC_DIR/$HOOK_NAME"
HOOK_DST="$CLAUDE_DIR/hooks/$HOOK_NAME"
LIMIT=4096

fail() { echo "Stopped. Your Claude settings were not changed: $*" >&2; exit 1; }

command -v jq >/dev/null 2>&1 || fail "jq is not installed."
[ -f "$HOOK_SRC" ] || fail "$HOOK_SRC is missing."
case "$HOOK_DST" in *[[:space:]]*) fail "the path $HOOK_DST has a space in it." ;; esac
if [ -e "$SETTINGS" ]; then
    jq -e 'type == "object"' "$SETTINGS" >/dev/null 2>&1 || fail "$SETTINGS is not valid JSON."
fi

# Warp's plugin script, to test against the real thing.
PLUGIN_SCRIPT=""
INSTALLED="$CLAUDE_DIR/plugins/installed_plugins.json"
if [ -f "$INSTALLED" ]; then
    while IFS= read -r p; do
        if [ -n "$p" ] && [ -f "$p/scripts/on-permission-request.sh" ]; then
            PLUGIN_SCRIPT="$p/scripts/on-permission-request.sh"
        fi
    done < <(jq -r '.plugins["warp@claude-code-warp"][]?.installPath // empty' "$INSTALLED" 2>/dev/null || true)
fi
if [ -z "$PLUGIN_SCRIPT" ]; then
    for p in "$CLAUDE_DIR"/plugins/cache/claude-code-warp/warp/*/scripts/on-permission-request.sh; do
        [ -f "$p" ] && PLUGIN_SCRIPT="$p"
    done
fi

# ---- 1. Self-test --------------------------------------------------------------------------
TEST_ENV=(env WARP_CLI_AGENT_PROTOCOL_VERSION=1 WARP_CLIENT_VERSION=v0.2026.11.20.00.00.stable_00 CLAUDE_CODE_VERSION=2.1.331)
make_input() {
    # $1 = number of plan lines
    local plan="## Plan" i=1
    while [ "$i" -le "$1" ]; do
        plan="$plan
$i. Update the iPad layout; keep the duo view; test portrait and landscape."
        i=$((i + 1))
    done
    jq -nc --arg plan "$plan" '{session_id: "selftest-0000", cwd: "/tmp/warposs-selftest", hook_event_name: "PermissionRequest",
        tool_name: "ExitPlanMode", tool_input: {plan: $plan, planFilePath: "/tmp/warposs-selftest/plan.md"}, permission_mode: "plan"}'
}
sequence_of() {
    # $1 = script, stdin = hook input; prints the terminalSequence it asks Claude to write (or nothing)
    local out
    out=$("${TEST_ENV[@]}" /bin/bash "$1") || return 1
    [ -z "$out" ] || printf '%s' "$out" | jq -r '.terminalSequence // empty'
}
bytes() { printf '%s' "$1" | LC_ALL=C wc -c | tr -d ' '; }

LONG_INPUT=$(make_input 120)
SHORT_INPUT=$(make_input 5)
HOOK_LONG=$(printf '%s' "$LONG_INPUT" | sequence_of "$HOOK_SRC") || fail "the hook failed on a long plan."
HOOK_SHORT=$(printf '%s' "$SHORT_INPUT" | sequence_of "$HOOK_SRC") || fail "the hook failed on a short plan."
[ -n "$HOOK_LONG" ] || fail "the hook sent nothing for a long plan."
[ "$(bytes "$HOOK_LONG")" -le "$LIMIT" ] || fail "the hook's message is over $LIMIT bytes."
printf '%s' "$HOOK_LONG" | grep -q '"event":"permission_request"' || fail "the hook's message is not a permission_request."
printf '%s' "$HOOK_LONG" | grep -q '"tool_name":"ExitPlanMode"' || fail "the hook's message lost the tool name."
[ -z "$HOOK_SHORT" ] || fail "the hook spoke for a short plan (it would arrive twice)."
if [ -n "$PLUGIN_SCRIPT" ]; then
    PLUGIN_LONG=$(printf '%s' "$LONG_INPUT" | sequence_of "$PLUGIN_SCRIPT") || fail "Warp's plugin script failed on a long plan."
    PLUGIN_SHORT=$(printf '%s' "$SHORT_INPUT" | sequence_of "$PLUGIN_SCRIPT") || fail "Warp's plugin script failed on a short plan."
    [ "$(bytes "$PLUGIN_LONG")" -gt "$LIMIT" ] || fail "Warp's plugin no longer sends long plans in full; this hook may not be needed."
    [ -n "$PLUGIN_SHORT" ] && [ "$(bytes "$PLUGIN_SHORT")" -le "$LIMIT" ] || fail "Warp's plugin did not send the short plan."
    echo "Self-test passed against Warp's plugin at $PLUGIN_SCRIPT"
else
    echo "Self-test passed (Warp's plugin was not found, so it was not compared)."
fi

# ---- 2. Backup -----------------------------------------------------------------------------
mkdir -p "$BACKUP_DIR" "$CLAUDE_DIR/hooks"
STAMP=$(date -u +%Y%m%d-%H%M%S)
BACKUP="(none: settings.json did not exist)"
if [ -e "$SETTINGS" ]; then
    BACKUP="$BACKUP_DIR/claude-settings-before-hook-install-$STAMP.json"
    [ -e "$BACKUP" ] && BACKUP="${BACKUP%.json}-$$.json"
    cp -p "$SETTINGS" "$BACKUP"
    echo "Backup: $BACKUP"
    BASE="$SETTINGS"
else
    BASE=""
fi

# ---- 3. Install ----------------------------------------------------------------------------
TMP="$SETTINGS.warposs-tmp"
{ if [ -n "$BASE" ]; then cat "$BASE"; else echo '{}'; fi; } | jq --arg cmd "$HOOK_DST" --arg name "$HOOK_NAME" '
    .hooks = (.hooks // {})
    | .hooks.PermissionRequest = (
        [ (.hooks.PermissionRequest // [])[]
          | select(any(.hooks[]?; (.command? // "") | contains($name)) | not) ]
        + [ { hooks: [ { type: "command", command: $cmd, timeout: 10 } ] } ]
      )' > "$TMP" || { rm -f "$TMP"; fail "could not build the new settings."; }

# The new file must equal the old one except for this hook's entry.
normalize='del(.hooks.PermissionRequest) | if .hooks == {} then del(.hooks) else . end'
others='[(.hooks.PermissionRequest // [])[] | select(any(.hooks[]?; (.command? // "") | contains($name)) | not)]'
OLD_REST=$({ if [ -n "$BASE" ]; then cat "$BASE"; else echo '{}'; fi; } | jq -S "$normalize")
NEW_REST=$(jq -S "$normalize" "$TMP")
OLD_PR=$({ if [ -n "$BASE" ]; then cat "$BASE"; else echo '{}'; fi; } | jq -S --arg name "$HOOK_NAME" "$others")
NEW_PR=$(jq -S --arg name "$HOOK_NAME" "$others" "$TMP")
OURS=$(jq --arg name "$HOOK_NAME" '[.hooks.PermissionRequest[] | select(any(.hooks[]?; (.command? // "") | contains($name)))] | length' "$TMP")
if [ "$OLD_REST" != "$NEW_REST" ] || [ "$OLD_PR" != "$NEW_PR" ] || [ "$OURS" != "1" ]; then
    rm -f "$TMP"
    fail "the new settings would change more than this one hook."
fi

cp "$HOOK_SRC" "$HOOK_DST.tmp" && chmod 755 "$HOOK_DST.tmp" && mv "$HOOK_DST.tmp" "$HOOK_DST"
if [ -L "$SETTINGS" ]; then
    cat "$TMP" > "$SETTINGS" && rm -f "$TMP"      # write through the link, keep it a link
else
    if [ -e "$SETTINGS" ]; then
        MODE=$(stat -c %a "$SETTINGS" 2>/dev/null || stat -f %Lp "$SETTINGS")
        chmod "$MODE" "$TMP"
    fi
    mv "$TMP" "$SETTINGS"
fi
if ! jq -e '.hooks.PermissionRequest | length > 0' "$SETTINGS" >/dev/null 2>&1; then
    echo "Warning: settings.json did not read back as expected. Restore it with: cp \"$BACKUP\" \"$SETTINGS\"" >&2
    exit 1
fi

echo "Installed. Hook: $HOOK_DST"
echo "Hook events in settings.json now: $(jq -r '.hooks | keys | join(", ")' "$SETTINGS")"
echo "Open Claude sessions pick it up on their own. To undo: bash $SRC_DIR/uninstall.sh"
