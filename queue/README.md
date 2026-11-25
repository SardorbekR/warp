# queue

A Claude Code plugin that holds the prompts you type while Claude is working and
sends them, one at a time, when Claude finishes.

## How it works

- **Enter while Claude works**: the prompt is held. A strip above the prompt box
  shows `Queued N · sends when Claude finishes` with every held prompt.
- **Claude finishes a turn**: the first held prompt is sent as your own message
  (the transcript marks it "Prompt from the queue plugin"). One prompt per
  finished turn, so each one gets its own answer.
- **Cmd+Enter while Claude works**: hands the prompt to Claude without stopping
  it, the way Enter worked before; Claude reads it at its next step. It needs
  the key binding below.
- **Enter while Claude is idle**: sends at once, as always.
- Slash commands, `!` shell commands, prompts with pasted images, and empty
  prompts are never held.

Buttons on the strip:

- **Send now**: stops the current turn, as Esc does, and sends the first held
  prompt.
- **Edit**: moves a held prompt back into the prompt box (the box must be empty).
  Press Enter to hold it again or Cmd+Enter to send it now.
- **✕**: removes one held prompt.
- **Send next** and **Clear** appear while the queue is paused.

To press a button with the keyboard, press Ctrl+X then Tab: the first button is
highlighted. Tab or the arrow keys move between buttons, Enter presses one, and
Esc goes back to the prompt box. Typing a digit or a letter never presses one.
A mouse click reaches the buttons only in Claude's fullscreen renderer
(`/tui fullscreen`); in the classic renderer Claude does not turn on mouse
reporting, so the terminal never sends it the click.

The queue pauses instead of sending when you stop Claude with Esc, when a turn
ends with an error, and when held prompts come back to a conversation: after
Claude restarts and the conversation is resumed (they are kept on disk for 14
days), after `/resume`, and after `/clear`, which moves them to the new
conversation. Typing a new prompt while Claude is idle also resumes a queue
paused by Esc or an error.

## Limits

- A held prompt is sent as plain text: `@file` mentions are not expanded.
- There is no "send this held prompt into the running turn" button. Use Edit and
  then Cmd+Enter, or Send now.
- Claude's own send-now keys (Ctrl+Enter, Ctrl+X Ctrl+S) are held like Enter when
  the box has text, because a plugin cannot tell them apart. To stop Claude and
  send what you typed, press Esc, then Enter.
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
