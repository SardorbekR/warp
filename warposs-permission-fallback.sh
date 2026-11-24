#!/bin/bash
# WarpOss: Claude Code PermissionRequest hook (runs next to Warp's own plugin hook).
#
# Claude Code (2.1.141+) writes a hook's `terminalSequence` to the terminal only when it is
# at most 4096 bytes; a longer one is dropped without a word. Warp's plugin (warp 2.4.0)
# puts the whole tool input in its permission_request message, so a long plan
# (ExitPlanMode) or a large Write/Edit never reaches WarpOss and the sidebar keeps showing
# the session as running while it waits for you.
#
# This hook rebuilds the plugin's message the same way to measure it, and only when it is
# too long sends a short copy: same event and summary, without the bulky tool input.
# Otherwise it prints nothing, so nothing arrives twice. It never answers the permission
# prompt and always exits 0.

LIMIT=4096
MIN_TERMINAL_SEQUENCE_VERSION="2.1.141"

# Same gate as the plugin (should-use-structured.sh): only inside a Warp build that reads
# structured notifications.
[ -n "${WARP_CLI_AGENT_PROTOCOL_VERSION:-}" ] || exit 0
[ -n "${WARP_CLIENT_VERSION:-}" ] || exit 0
case "$WARP_CLIENT_VERSION" in
    *stable*) LAST_BROKEN="v0.2026.03.25.08.24.stable_05" ;;
    *preview*) LAST_BROKEN="v0.2026.03.25.08.24.preview_05" ;;
    *) LAST_BROKEN="" ;;
esac
if [ -n "$LAST_BROKEN" ] && [[ ! "$WARP_CLIENT_VERSION" > "$LAST_BROKEN" ]]; then
    exit 0
fi
command -v jq >/dev/null 2>&1 || exit 0

version_at_least() {
    local a b i av bv
    IFS=. read -ra a <<< "$1"
    IFS=. read -ra b <<< "$2"
    for ((i = 0; i < ${#b[@]}; i++)); do
        av="${a[i]:-0}"
        bv="${b[i]:-0}"
        [[ "$av" =~ ^[0-9]+$ ]] || av=0
        if ((av > bv)); then return 0; fi
        if ((av < bv)); then return 1; fi
    done
    return 0
}

# The plugin only goes through `terminalSequence` (and its 4096-byte limit) on a Claude Code
# it knows is new enough, or when it can't write to the terminal itself.
CC_VERSION=$(printf '%s' "${CLAUDE_CODE_VERSION:-}" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
if [ -n "$CC_VERSION" ]; then
    version_at_least "$CC_VERSION" "$MIN_TERMINAL_SEQUENCE_VERSION" || exit 0
elif (: > /dev/tty) 2>/dev/null; then
    exit 0
fi

INPUT=$(cat)
[ -n "$INPUT" ] || exit 0
printf '%s' "$INPUT" | jq -e 'type == "object"' >/dev/null 2>&1 || exit 0

# Protocol version, as the plugin negotiates it.
PROTOCOL_VERSION=1
if [ "${WARP_CLI_AGENT_PROTOCOL_VERSION:-1}" -lt 1 ] 2>/dev/null; then
    PROTOCOL_VERSION="$WARP_CLI_AGENT_PROTOCOL_VERSION"
fi

SESSION_ID=$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
CWD=$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
PROJECT=""
[ -n "$CWD" ] && PROJECT=$(basename "$CWD")
TOOL_NAME=$(printf '%s' "$INPUT" | jq -r '.tool_name // "unknown"' 2>/dev/null)
TOOL_INPUT=$(printf '%s' "$INPUT" | jq -c '.tool_input // {}' 2>/dev/null)
[ -z "$TOOL_INPUT" ] && TOOL_INPUT='{}'
TOOL_PREVIEW=$(printf '%s' "$INPUT" | jq -r '(.tool_input | if .command then .command elif .file_path then .file_path else (tostring | .[0:80]) end) // ""' 2>/dev/null)
SUMMARY="Wants to run $TOOL_NAME"
if [ -n "$TOOL_PREVIEW" ]; then
    if [ ${#TOOL_PREVIEW} -gt 120 ]; then
        TOOL_PREVIEW="${TOOL_PREVIEW:0:117}..."
    fi
    SUMMARY="$SUMMARY: $TOOL_PREVIEW"
fi

build_sequence() {
    # $1 = summary, $2 = tool input (JSON)
    local body
    body=$(jq -nc \
        --argjson v "$PROTOCOL_VERSION" \
        --arg agent "claude" \
        --arg event "permission_request" \
        --arg session_id "$SESSION_ID" \
        --arg cwd "$CWD" \
        --arg project "$PROJECT" \
        --arg summary "$1" \
        --arg tool_name "$TOOL_NAME" \
        --argjson tool_input "$2" \
        '{v:$v, agent:$agent, event:$event, session_id:$session_id, cwd:$cwd, project:$project} + $ARGS.named' 2>/dev/null) || return 1
    [ -n "$body" ] || return 1
    printf '\033]777;notify;%s;%s\007' "warp://cli-agent" "$body"
}

byte_length() {
    printf '%s' "$1" | LC_ALL=C wc -c | tr -d ' '
}

# When the rebuild itself fails, the tool input is too big for jq's command line (about 1 MB on
# macOS, 128 KB per argument on Linux). The plugin passes it the same way, so its message failed
# too, and the short copy is the only one.
if PLUGIN_SEQUENCE=$(build_sequence "$SUMMARY" "$TOOL_INPUT"); then
    [ "$(byte_length "$PLUGIN_SEQUENCE")" -gt "$LIMIT" ] || exit 0
fi

# The short copy. Semicolons become commas: WarpOss 9.3 and older cut a message with more
# than 12 of them.
SHORT_SUMMARY=${SUMMARY//;/,}
SHORT_INPUT=$(printf '%s' "$INPUT" | jq -c '
    (.tool_input // {}) as $input
    | if ($input | type) == "object" then $input else {} end
    | {command, file_path}
    | with_entries(select(.value != null) | .value |= (tostring | .[0:200] | gsub(";"; ",")))
' 2>/dev/null)
[ -z "$SHORT_INPUT" ] && SHORT_INPUT='{}'

SHORT_SEQUENCE=$(build_sequence "$SHORT_SUMMARY" "$SHORT_INPUT") || exit 0
if [ "$(byte_length "$SHORT_SEQUENCE")" -gt "$LIMIT" ]; then
    SHORT_SEQUENCE=$(build_sequence "Wants to run $TOOL_NAME" '{}') || exit 0
    [ "$(byte_length "$SHORT_SEQUENCE")" -le "$LIMIT" ] || exit 0
fi

jq -nc --arg seq "$SHORT_SEQUENCE" '{terminalSequence: $seq}'
exit 0
