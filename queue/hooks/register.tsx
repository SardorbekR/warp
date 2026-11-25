import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { QueuedPrompt, QueuePause } from '../types'

// Cmd+Enter while Claude works holds the prompt here: it is bound to
// chat:queueSubmit, which arrives with `wait` set, and the prompt is sent as
// the person's own words when the turn ends. Enter goes to Claude as it always
// does: into the running turn, read at Claude's next step.

type $ = EngineInterface

// Change the tag when QueuedPrompt's fields change: a reload then drops what
// an earlier version kept instead of misreading it.
const items = atom({ plugin: 'queue', key: 'items' } as const, [] as QueuedPrompt[], {
  shape: 'items-1',
})
const paused = atom({ plugin: 'queue', key: 'paused' } as const, null as QueuePause | null)

const DROP_NOTICE = 'Queued'
const KEEP_DAYS = 14

const PAUSE_TEXT: Record<QueuePause, string> = {
  stopped: 'paused, you stopped Claude',
  error: 'paused, Claude stopped with an error',
  restored: 'paused, kept from before Claude restarted',
  resumed: 'paused, kept from before /resume',
  cleared: 'paused, kept from before /clear',
}

type Saved = { savedAt: number; items: QueuedPrompt[] }

// Module values start over at a reload; the queue itself lives in $.state.
let runningTurn: string | null = null
let isSending = false
let putBack: { text: string; at: number } | null = null
let counter = 0
// Where Edit took a prompt from: the next prompt held from the box goes back
// there. Any other submit from the box forgets it.
let editSpot: EditSpot | null = null

type EditSpot = { afterId: string | undefined; beforeId: string | undefined }

// After the prompt that stood before it, else before the one that stood after
// it; with both gone, first if it was first, otherwise last.
function placeAt(list: QueuedPrompt[], held: QueuedPrompt, spot: EditSpot): QueuedPrompt[] {
  const after = list.findIndex(one => one.id === spot.afterId)
  if (after >= 0) {
    return [...list.slice(0, after + 1), held, ...list.slice(after + 1)]
  }
  const before = list.findIndex(one => one.id === spot.beforeId)
  if (before >= 0) {
    return [...list.slice(0, before), held, ...list.slice(before)]
  }
  return spot.afterId === undefined ? [held, ...list] : [...list, held]
}

async function storeKey($: $) {
  return `queue:${await $.session.id()}`
}

// Keeps a copy outside the session so a closed tab gets its queue back.
// Best effort: a failed save never stops the queue itself.
async function persist($: $) {
  try {
    const list = await read($, items)
    const key = await storeKey($)
    if (list.length === 0) {
      await $.store.delete(key)
    } else {
      const saved: Saved = { savedAt: await $.clock.now(), items: list }
      await $.store.set(key, saved)
    }
  } catch {
    // The live queue in $.state is unaffected.
  }
}

async function setItems($: $, fn: (list: QueuedPrompt[]) => QueuedPrompt[]) {
  await update($, items, fn)
  if ((await read($, items)).length === 0) {
    await update($, paused, () => null)
  }
  await persist($)
}

async function putFirstBack($: $, first: QueuedPrompt) {
  await update($, items, list => [first, ...list.filter(one => one.id !== first.id)])
  await update($, paused, () => 'error')
  await persist($)
}

async function sendFirst($: $) {
  if (isSending) {
    return
  }
  const first = (await read($, items))[0]
  if (first === undefined) {
    return
  }
  isSending = true
  await setItems($, list => list.filter(one => one.id !== first.id))
  // Not awaited inside a hook: it resolves once the prompt has entered.
  void $.prompt.submit({ text: first.text, asUser: true }).then(
    async result => {
      isSending = false
      if (result.drop !== undefined) {
        await putFirstBack($, first)
      }
    },
    async () => {
      isSending = false
      await putFirstBack($, first)
    },
  )
}

// A resumed conversation gets back what was waiting when it closed, paused.
async function restoreSaved($: $, why: 'restored' | 'resumed') {
  const saved = (await $.store.get(await storeKey($))) as Saved | undefined
  if (saved === undefined || saved.items.length === 0 || (await read($, items)).length > 0) {
    return
  }
  // The pause goes first so the strip never shows the restored list as about
  // to send.
  await update($, paused, () => why)
  try {
    await update($, items, () => saved.items)
  } catch {
    await update($, paused, () => null)
  }
}

// After /clear or /resume the process goes on under another session id, and
// this plugin's $.state starts empty there. session.end still runs under the
// old id, so the move waits for a timer, which runs under the new one. /clear
// carries the held prompts over, paused; /resume gets that conversation's own.
async function afterSwitch($: $, fromId: string, carried: QueuedPrompt[], attempt: number) {
  try {
    if ((await $.session.id()) === fromId) {
      if (attempt < 4) {
        $.clock.after(250, () => afterSwitch($, fromId, carried, attempt + 1))
      }
      return
    }
    if (carried.length === 0) {
      await restoreSaved($, 'resumed')
      return
    }
    await update($, paused, () => 'cleared')
    try {
      await update($, items, list => [...carried, ...list.filter(one => !carried.some(other => other.id === one.id))])
    } catch {
      await update($, paused, () => null)
      return
    }
    await persist($)
    await $.store.delete(`queue:${fromId}`)
  } catch {
    // What was held stays saved under the old conversation.
  }
}

// A held prompt sent while Claude works goes in as a note the model reads at
// its next request, without stopping the turn. A plugin has no way to add the
// person's own message to a running turn, so Claude Code labels the note as the
// plugin's ("not from the user"); the text says whose words they are.
const NOTE_INTRO =
  'The person typed the message below while you were working. It waited in their queue, and they just chose to send it now:'

// The note sent during the running turn, until a model request carries it.
let note: { turnId: string; one: QueuedPrompt; isCarried: boolean } | null = null

// Takes the prompt off the list and adds it to the running turn as a note.
// Answers why it could not, or undefined.
async function sendAsNote($: $, one: QueuedPrompt, turnId: string): Promise<string | undefined> {
  await setItems($, list => list.filter(other => other.id !== one.id))
  try {
    const appended = await $.session.append({
      message: { type: 'user', content: [{ type: 'text', text: `${NOTE_INTRO}\n\n${one.text}` }] },
    })
    if (appended.deny !== undefined) {
      throw new Error(appended.deny)
    }
  } catch {
    await update($, items, list => [one, ...list.filter(other => other.id !== one.id)])
    await persist($)
    return "Couldn't hand it to Claude now; it is back at the top of the queue."
  }
  note = { turnId, one, isCarried: false }
  return undefined
}

// Sends the first held prompt: into the running turn as a note, or as the
// person's prompt when Claude is idle.
async function sendNow($: $, isWorking = true): Promise<string | undefined> {
  const first = (await read($, items))[0]
  if (first === undefined) {
    return undefined
  }
  const turnId = runningTurn
  if (turnId === null || !isWorking) {
    await update($, paused, () => null)
    await sendFirst($)
    return undefined
  }
  return sendAsNote($, first, turnId)
}

// Moves a held prompt back into the prompt box; the next prompt held from the
// box goes back to its place. Answers why it could not, or undefined.
async function editHeld($: $, one: QueuedPrompt): Promise<string | undefined> {
  const box = await $.prompt.read()
  if (box.text.trim() !== '') {
    return 'Empty the prompt box first, then press Edit'
  }
  const all = await read($, items)
  const at = all.findIndex(other => other.id === one.id)
  if (at < 0) {
    return undefined
  }
  await setItems($, rest => rest.filter(other => other.id !== one.id))
  editSpot = { afterId: all[at - 1]?.id, beforeId: all[at + 1]?.id }
  await $.prompt.fill({ text: one.text, mode: 'replace' })
  return undefined
}

// "/send-now 2" names held prompt 2; nothing after the name means 1.
type Picked = { one: QueuedPrompt; error?: undefined } | { one?: undefined; error: string }

function short(text: string) {
  const line = text.replace(/\s*\n\s*/g, ' ⏎ ')
  return line.length > 60 ? `${line.slice(0, 59)}…` : line
}

async function pickHeld($: $, command: string, args: string): Promise<Picked> {
  const list = await read($, items)
  const typed = args.trim()
  if (list.length === 0) {
    return { error: 'Nothing is queued.' }
  }
  if (typed !== '' && !/^[0-9]+$/.test(typed)) {
    return { error: `Type /${command} for the first held prompt, or /${command} 2 for the second.` }
  }
  const index = typed === '' ? 0 : Number(typed) - 1
  const one = list[index]
  if (one === undefined) {
    return { error: `There is no held prompt ${typed}. ${list.length} ${list.length === 1 ? 'is' : 'are'} queued.` }
  }
  return { one }
}

const COMMANDS = [
  {
    name: 'send-now',
    description: 'Give Claude the first held prompt now, without stopping it (or /send-now 2 for the second)',
    argumentHint: '[number]',
    immediate: true,
  },
  {
    name: 'edit-queued',
    description: 'Put the first held prompt back in the prompt box (or /edit-queued 2 for the second)',
    argumentHint: '[number]',
    immediate: true,
  },
] as const

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const started = await next(e)
    for (const command of COMMANDS) {
      try {
        await $.command.register(command)
      } catch {
        // The strip's buttons still work without the typed commands.
      }
    }
    const now = await $.clock.now()
    const key = await storeKey($)
    for (const other of await $.store.keys()) {
      if (other === key || !other.startsWith('queue:')) {
        continue
      }
      const saved = (await $.store.get(other)) as Saved | undefined
      if (saved === undefined || now - saved.savedAt > KEEP_DAYS * 86_400_000) {
        await $.store.delete(other)
      }
    }
    await restoreSaved($, 'restored')

    return started
  })

  on('session.end', async ($, e, next) => {
    editSpot = null
    note = null
    const carried = e.reason === 'clear' ? await read($, items) : []
    const ended = await next(e)
    if (e.reason === 'clear' || e.reason === 'resume') {
      $.clock.after(0, () => afterSwitch($, e.sessionId, carried, 0))
    }

    return ended
  })

  on('prompt.submit', async ($, e, next) => {
    if (e.origin.kind !== 'composer') {
      return next(e)
    }
    const spot = editSpot
    editSpot = null
    if (e.turnId === undefined) {
      // Typed while Claude is idle: it goes now, and a queue paused by Esc
      // or an error goes on after it.
      const pause = await read($, paused)
      if (pause === 'stopped' || pause === 'error') {
        await update($, paused, () => null)
      }
      return next(e)
    }
    const isPlainText =
      e.wait &&
      e.command === undefined &&
      e.isMeta === undefined &&
      (e.attachments?.length ?? 0) === 0 &&
      e.text.trim() !== ''
    if (!isPlainText) {
      return next(e)
    }
    // Nothing after the queue write may throw: a failed hook here is skipped,
    // which would send the prompt now while it also waits in the queue.
    const now = await $.clock.now()
    counter += 1
    const held: QueuedPrompt = { id: `${now}-${counter}`, text: e.text }
    try {
      await update($, items, list => (spot === null ? [...list, held] : placeAt(list, held, spot)))
    } catch {
      // Not queued, so it goes to Claude as today rather than being lost.
      return next(e)
    }
    await persist($)
    // Claude puts a dropped prompt back in the box; prompt.change takes it out.
    putBack = { text: e.text, at: now }

    return { drop: DROP_NOTICE }
  })

  on('prompt.change', async ($, e, next) => {
    const changed = await next(e)
    const held = putBack
    if (held === null || e.origin.kind !== 'engine') {
      return changed
    }
    putBack = null
    const isRecent = (await $.clock.now()) - held.at < 3000
    if (isRecent && e.text.startsWith(held.text)) {
      // Keeps anything typed after Enter, in case the person kept typing.
      await $.prompt.fill({ text: e.text.slice(held.text.length), mode: 'replace' })
    }

    return changed
  })

  on('command.run', { command: 'send-now' }, async ($, e) => {
    const picked = await pickHeld($, 'send-now', e.args)
    if (picked.one === undefined) {
      return { text: picked.error, isTextDisplayOnly: true }
    }
    const one = picked.one
    const turnId = runningTurn
    if (turnId !== null) {
      const why = await sendAsNote($, one, turnId)
      const sent = `Given to Claude without stopping it; it reads it at its next step: ${short(one.text)}`

      return { text: why ?? sent, isTextDisplayOnly: true }
    }
    // Claude is idle: it goes as the person's prompt. A prompt submitted while
    // this command is still running is refused, so it waits for the command.
    await setItems($, list => [one, ...list.filter(other => other.id !== one.id)])
    await update($, paused, () => null)
    $.clock.after(0, () => sendFirst($))

    return { text: `Sending now: ${short(one.text)}`, isTextDisplayOnly: true }
  })

  on('command.run', { command: 'edit-queued' }, async ($, e) => {
    const picked = await pickHeld($, 'edit-queued', e.args)
    if (picked.one === undefined) {
      return { text: picked.error, isTextDisplayOnly: true }
    }
    const why = await editHeld($, picked.one)

    const back = 'Back in the prompt box. Cmd+Enter holds it again in its place; Enter gives it to Claude now.'

    return { text: why ?? back, isTextDisplayOnly: true }
  })

  // A request made after the note was added carries it to the model.
  on('turn.step', async function* ($, e, next) {
    if (e.agentId === undefined && note !== null && note.turnId === e.turnId) {
      note.isCarried = true
    }

    return yield* next(e)
  })

  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) {
      runningTurn = e.turnId
    }

    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    const completed = await next(e)
    if (e.agentId !== undefined) {
      return completed
    }
    runningTurn = null
    const sent = note
    note = null
    if (sent !== null && !sent.isCarried) {
      // The turn ended before another request took the note, so the prompt goes
      // back to the top and is sent as the person's own when the queue goes on.
      await update($, items, list => [sent.one, ...list.filter(other => other.id !== sent.one.id)])
      await persist($)
    }
    const list = await read($, items)
    if (list.length === 0) {
      return completed
    }
    if (e.reason === 'answer') {
      if ((await read($, paused)) === null) {
        await sendFirst($)
      }
    } else {
      await update($, paused, () => (e.reason === 'aborted' ? 'stopped' : 'error'))
    }

    return completed
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const list = await read($, items)
    if (e.props.hasSurvey || list.length === 0) {
      return next(e)
    }
    const pause = await read($, paused)
    const isWorking = e.props.isWorking
    const { Box, Button, Text } = $.ui.resolve(e)

    const sendNext = async () => {
      await update($, paused, () => null)
      if (!isWorking) {
        await sendFirst($)
      }
    }
    const sendNowPressed = async () => {
      const why = await sendNow($, isWorking)
      $.ui.toast(why ?? (isWorking ? 'Given to Claude; it reads it at its next step' : 'Sent'))
    }
    const edit = async (one: QueuedPrompt) => {
      const why = await editHeld($, one)
      if (why !== undefined) {
        $.ui.toast(why)
      }
    }
    const remove = (one: QueuedPrompt) => setItems($, all => all.filter(other => other.id !== one.id))
    const clear = () => setItems($, () => [])

    return (
      <Box flexDirection="column">
        <Box flexDirection="row" gap={1}>
          <Text bold color="warning">
            Queued {list.length}
          </Text>
          <Text dimColor>· {pause === null ? 'sends when Claude finishes' : PAUSE_TEXT[pause]}</Text>
          {pause !== null && <Button key="send-next" label="Send next" variant="primary" onPress={sendNext} />}
          {pause !== null && <Button key="clear" label="Clear" onPress={clear} />}
          {pause === null && isWorking && (
            <Button key="send-now" label="Send now" variant="primary" onPress={sendNowPressed} />
          )}
        </Box>
        {list.map((one, index) => (
          <Box key={`row-${one.id}`} flexDirection="row" gap={1}>
            <Text color="warning">{String(index + 1)}</Text>
            <Box flexGrow={1} flexShrink={1}>
              <Text wrap="truncate-end">{one.text.replace(/\s*\n\s*/g, ' ⏎ ')}</Text>
            </Box>
            <Button key={`edit-${one.id}`} label="Edit" onPress={() => edit(one)} />
            <Button key={`remove-${one.id}`} label="✕" onPress={() => remove(one)} />
          </Box>
        ))}
      </Box>
    )
  })
}
