import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { QueuedPrompt, QueuePause } from '../types'

// Enter while Claude works holds the prompt here; it is sent as the person's
// own words when the turn ends. Cmd+Enter is bound to chat:queueSubmit, which
// arrives with `wait` set and goes straight to Claude as today.

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
let isStopAndSend = false
let isSending = false
let putBack: { text: string; at: number } | null = null
let counter = 0

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

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const started = await next(e)
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
      !e.wait &&
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
      await update($, items, list => [...list, held])
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
    const list = await read($, items)
    if (isStopAndSend) {
      isStopAndSend = false
      await update($, paused, () => null)
      await sendFirst($)
      return completed
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
    const isWorking = e.props.isWorking
    const { Box, Button, Text } = $.ui.resolve(e)

    const sendNext = async () => {
      await update($, paused, () => null)
      if (!isWorking) {
        await sendFirst($)
      }
    }
    const stopAndSend = async () => {
      if (isWorking && runningTurn !== null) {
        isStopAndSend = true
        try {
          await $.turn.abort({ turnId: runningTurn })
        } catch {
          isStopAndSend = false
          await sendFirst($)
        }
      } else {
        await sendFirst($)
      }
    }
    const edit = async (one: QueuedPrompt) => {
      const box = await $.prompt.read()
      if (box.text.trim() !== '') {
        $.ui.toast('Empty the prompt box first, then press Edit')
        return
      }
      await setItems($, all => all.filter(other => other.id !== one.id))
      await $.prompt.fill({ text: one.text, mode: 'replace' })
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
            <Button key="send-now" label="Send now" variant="primary" onPress={stopAndSend} />
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
