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
// Held prompts the person chose to send while no note could go in: they go
// at the first moment one can, or when the turn ends (see NOTE_INTRO).
const marked = atom({ plugin: 'queue', key: 'marked' } as const, [] as string[])

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
// The main loop's tool calls running now. Their results go back to Claude in a
// request that is not made yet, and a note added meanwhile goes with it.
const toolsRunning = new Set<string>()
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
  const list = await read($, items)
  if (list.length === 0) {
    await update($, paused, () => null)
  }
  if ((await read($, marked)).some(id => !list.some(one => one.id === id))) {
    await update($, marked, ids => ids.filter(id => list.some(one => one.id === id)))
  }
  await persist($)
}

async function putFirstBack($: $, first: QueuedPrompt) {
  await update($, items, list => [first, ...list.filter(one => one.id !== first.id)])
  await update($, paused, () => 'error')
  await update($, marked, () => [])
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

// A held prompt sent while Claude works reaches Claude at its next model
// request, without stopping the turn. A plugin has no way to add the person's
// own message to a running turn, so it goes in as a note, which Claude Code
// labels as the plugin's ("not from the user"); the text says whose words
// they are.
//
// A note waits for the next request, and one added after that request was put
// together, or after the turn's last one, waits for the next turn instead. So
// a note goes in only while another request is sure to come and not yet put
// together: while a tool of the main loop runs, and right after a response
// that asked for tools. At any other time the prompt is marked and goes at the
// first such moment, or as the person's own prompt when the turn ends.
const NOTE_INTRO =
  'The person typed the message below while you were working. It waited in their queue, and they just chose to send it now:'

// Takes the prompt off the list and adds it to the running turn as a note.
// Answers why it could not, or undefined.
async function giveAsNote($: $, one: QueuedPrompt): Promise<string | undefined> {
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
  return undefined
}

type Handed = { why?: string; isMarked: boolean }

// Gives Claude a held prompt while it works: as a note now while a tool runs,
// else marked and moved up behind the prompts already marked.
async function sendWhileWorking($: $, one: QueuedPrompt): Promise<Handed> {
  await update($, paused, () => null)
  if (toolsRunning.size > 0) {
    return { why: await giveAsNote($, one), isMarked: false }
  }
  const before = await read($, marked)
  await update($, marked, ids => [...ids.filter(id => id !== one.id), one.id])
  await setItems($, list => {
    const rest = list.filter(other => other.id !== one.id)
    const first = rest.filter(other => before.includes(other.id))

    return [...first, one, ...rest.filter(other => !before.includes(other.id))]
  })
  return { isMarked: true }
}

// The marked prompts, as notes in the queue's order. Only while another
// request is sure to come and not yet put together.
async function handOverMarked($: $) {
  const ids = await read($, marked)
  if (ids.length === 0) {
    return
  }
  await update($, marked, () => [])
  for (const one of (await read($, items)).filter(other => ids.includes(other.id))) {
    const why = await giveAsNote($, one)
    if (why !== undefined) {
      $.ui.toast(why)
    }
  }
}

const GIVEN = 'Given to Claude without stopping it; it reads it at its next step'
const MARKED = 'Claude gets it when its current step ends, without stopping it'

// The Send now button: the first held prompt not already on its way. While
// Claude is idle it goes as the person's prompt. Answers what happened.
async function sendNow($: $, isWorking: boolean): Promise<string | undefined> {
  const list = await read($, items)
  if (runningTurn === null || !isWorking) {
    if (list.length === 0) {
      return undefined
    }
    await update($, paused, () => null)
    await sendFirst($)
    return 'Sent'
  }
  const ids = await read($, marked)
  const one = list.find(other => !ids.includes(other.id))
  if (one === undefined) {
    return undefined
  }
  const handed = await sendWhileWorking($, one)
  return handed.why ?? (handed.isMarked ? MARKED : GIVEN)
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
    toolsRunning.clear()
    await update($, marked, () => [])
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
    if (runningTurn !== null) {
      if ((await read($, marked)).includes(one.id)) {
        return { text: `${MARKED}: ${short(one.text)}`, isTextDisplayOnly: true }
      }
      const handed = await sendWhileWorking($, one)
      const sent = `${handed.isMarked ? MARKED : GIVEN}: ${short(one.text)}`

      return { text: handed.why ?? sent, isTextDisplayOnly: true }
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

  // A response that asks for tools is followed by a request with their
  // results, put together only after they ran: the prompts marked while Claude
  // wrote go in with it.
  on('turn.step', async function* ($, e, next) {
    const response = yield* next(e)
    if (e.agentId === undefined && response.toolUses.length > 0 && runningTurn === e.turnId) {
      try {
        await handOverMarked($)
      } catch {
        // Still held: they go as the person's prompts when the turn ends.
      }
    }

    return response
  })

  // While a tool of the main loop runs, a prompt sent now goes in as a note.
  on('tool.call', async ($, e, next) => {
    if (e.agentId !== undefined || runningTurn === null) {
      return next(e)
    }
    const id = e.tool_use_id
    toolsRunning.add(id)
    try {
      await handOverMarked($)
    } catch {
      // Still held: they go as the person's prompts when the turn ends.
    }
    // The call's own promise goes back untouched, so the tool runs and fails
    // exactly as it would without this hook.
    const called = next(e)
    called.then(
      () => toolsRunning.delete(id),
      () => toolsRunning.delete(id),
    )

    return called
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
    toolsRunning.clear()
    const list = await read($, items)
    if (list.length === 0 || e.reason !== 'answer') {
      // Stopped or failed: what was marked stays held, at the top.
      await update($, marked, () => [])
    }
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
    const ids = await read($, marked)
    const isWorking = e.props.isWorking
    const canSendNow = pause === null && isWorking && list.some(one => !ids.includes(one.id))
    const { Box, Button, Text } = $.ui.resolve(e)

    const sendNext = async () => {
      await update($, paused, () => null)
      if (!isWorking) {
        await sendFirst($)
      }
    }
    const sendNowPressed = async () => {
      const what = await sendNow($, isWorking)
      if (what !== undefined) {
        $.ui.toast(what)
      }
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
          {canSendNow && <Button key="send-now" label="Send now" variant="primary" onPress={sendNowPressed} />}
        </Box>
        {list.map((one, index) => (
          <Box key={`row-${one.id}`} flexDirection="row" gap={1}>
            <Text color="warning">{String(index + 1)}</Text>
            <Box flexGrow={1} flexShrink={1}>
              <Text wrap="truncate-end">{one.text.replace(/\s*\n\s*/g, ' ⏎ ')}</Text>
            </Box>
            {isWorking && ids.includes(one.id) && <Text dimColor>sends after this step</Text>}
            <Button key={`edit-${one.id}`} label="Edit" onPress={() => edit(one)} />
            <Button key={`remove-${one.id}`} label="✕" onPress={() => remove(one)} />
          </Box>
        ))}
      </Box>
    )
  })
}
