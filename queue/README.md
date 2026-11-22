# queue

A Claude Code plugin that holds the prompts you type while Claude is working and
sends them, one at a time, when Claude finishes.

## How it works

- **Enter while Claude works**: the prompt is held. A strip above the prompt box
  shows `Queued N · sends when Claude finishes` with every held prompt.
- **Claude finishes a turn**: the first held prompt is sent as your own message
  (the transcript marks it "Prompt from the queue plugin"). One prompt per
  finished turn, so each one gets its own answer.
- **Cmd+Enter while Claude works**: sends now, the way Enter worked before. It
  needs the key binding below.
- **Enter while Claude is idle**: sends at once, as always.
- Slash commands, `!` shell commands, prompts with pasted images, and empty
  prompts are never held.

Buttons on the strip:

- **Stop & send 1**: stops the current turn and sends the first held prompt.
- **Edit**: moves a held prompt back into the prompt box (the box must be empty).
  Press Enter to hold it again or Cmd+Enter to send it now.
- **✕**: removes one held prompt.
- **Send next** and **Clear** appear while the queue is paused.

The queue pauses instead of sending when you stop Claude with Esc, when a turn
ends with an error, and when held prompts come back to a conversation: after
Claude restarts and the conversation is resumed (they are kept on disk for 14
days), after `/resume`, and after `/clear`, which moves them to the new
conversation. Typing a new prompt while Claude is idle also resumes a queue
paused by Esc or an error.

## Limits

- A held prompt is sent as plain text: `@file` mentions are not expanded.
- There is no "send this held prompt into the running turn" button. Use Edit and
  then Cmd+Enter, or Stop & send.
- It covers Claude Code only. Codex has its own queue (Tab).

## Key binding

Add this to `~/.claude/keybindings.json` so Cmd+Enter sends now:

```json
{
  "bindings": [
    { "context": "Chat", "bindings": { "cmd+enter": "chat:queueSubmit" } }
  ]
}
```

## Install from a local folder

```sh
claude plugin marketplace add /path/to/claude-mods
claude plugin install queue@sardor-mods
```

New sessions load it on start. A session that was already running loads it after
you type `/reload-plugins` in it.

Remove it with `claude plugin uninstall queue@sardor-mods`.
