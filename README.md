# WarpOss "needs you" fallback hook for Claude Code

Claude Code (2.1.141 and newer, checked up to 2.1.331) writes a hook's `terminalSequence` to the
terminal only when it is at most 4096 bytes and drops a longer one without a word. Warp's Claude
plugin (warp 2.4.0) puts the whole tool input into its `permission_request` message, so a long plan
(ExitPlanMode), a large Write/Edit or a long Bash command never reaches WarpOss, and the sidebar keeps
the spinner while Claude waits for you.

`warposs-permission-fallback.sh` runs next to the plugin's PermissionRequest hook. It rebuilds the
plugin's message to measure it, and only when it is too long sends a short copy (same event, session
and summary, without the bulky tool input). Otherwise it prints nothing, so nothing arrives twice. It
never answers the permission prompt and always exits 0.

- `bash install.sh` tests the hook against the installed plugin, backs up `~/.claude/settings.json`,
  copies the hook to `~/.claude/hooks/` and adds one PermissionRequest entry. Open sessions pick it up.
- `bash uninstall.sh` removes the entry and the script (with a backup first).
- `python3 -I test_hook.py warposs-permission-fallback.sh <plugin>/scripts/on-permission-request.sh /bin/bash`
  runs the script tests.
