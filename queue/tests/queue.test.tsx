import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const BAND = {
  plugin: 'queue',
  component: 'AbovePrompt',
  requestId: 'above-prompt',
  props: {
    hasSurvey: false,
    isCollapsed: false,
    isWorking: true,
    maxRows: 12,
    bodyColumns: 80,
    scroll: { offset: 0, bodyRows: 11 },
    view: {},
  },
} as const

const typedWhileBusy = { turnId: 't1', wait: false, origin: { kind: 'composer' } } as const

// Stands for the engine beneath the plugin: records what reaches Claude.
function engine(on: On) {
  const entered: string[] = []
  mock.clock(on, { now: 1_000 })
  mock.store(on)
  on('session.id', () => ({ value: 'session-1' }))
  on('prompt.submit', (_, e) => {
    entered.push(e.text)
    return { text: e.text }
  })
  on('turn.complete', () => ({ text: '' }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  return entered
}

test('Enter while Claude works is held; Cmd+Enter and idle prompts go through', async ($, on) => {
  const entered = engine(on)

  expect(await $.prompt.submit({ text: 'later', ...typedWhileBusy })).toEqual({ drop: 'Queued' })
  await $.prompt.submit({ text: 'now', turnId: 't1', wait: true, origin: { kind: 'composer' } })
  await $.prompt.submit({ text: 'idle', wait: false, origin: { kind: 'composer' } })
  await $.prompt.submit({ text: '/model', ...typedWhileBusy, command: { name: 'model', args: '' } })
  await $.prompt.submit({ text: 'from a schedule', turnId: 't1', wait: false, origin: { kind: 'scheduled-trigger' } })

  expect(entered).toEqual(['now', 'idle', '/model', 'from a schedule'])
})

test('a finished turn sends the first held prompt as the person, one per turn', async ($, on) => {
  const entered = engine(on)
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })

  await $.turn.complete({ turnId: 't1', reason: 'answer', isAborted: false, answer: '', durationMs: 1, usage: {} } as never)

  expect(entered).toEqual(['first'])
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /Queued 1/ }))?.text).toBe('Queued 1')
  expect(await ui.find({ type: 'Text', text: 'second' })).toBeDefined()
  await ui.unmount()
})

test('Esc pauses the queue; Send next sends it', async ($, on) => {
  const entered = engine(on)
  await $.prompt.submit({ text: 'held', ...typedWhileBusy })

  await $.turn.complete({ turnId: 't1', reason: 'aborted', isAborted: true, answer: '', durationMs: 1, usage: {} } as never)
  expect(entered).toEqual([])

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, isWorking: false } })
  expect(await ui.find({ type: 'Text', text: /you stopped Claude/ })).toBeDefined()
  await ui.press({ key: 'send-next' })
  expect(entered).toEqual(['held'])
  await ui.unmount()
})

test('the strip removes a prompt', async ($, on) => {
  engine(on)
  await $.prompt.submit({ text: 'keep me', ...typedWhileBusy })
  await $.prompt.submit({ text: 'drop me', ...typedWhileBusy })

  for (const surface of ['terminal', 'desktop'] as const) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect(await ui.find({ type: 'Text', text: 'drop me' })).toBeDefined()
    await ui.unmount()
  }
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const remove = (await ui.findAll({ type: 'Button', text: '✕' }))[1]
  expect(remove).toBeDefined()
  await ui.press({ key: remove?.key ?? '' })
  expect(await ui.find({ type: 'Text', text: 'drop me' })).toBeUndefined()
  await ui.unmount()

})

test('a resumed session gets its saved queue back, paused', async ($, on) => {
  mock.clock(on, { now: 5_000 })
  mock.store(on, { 'queue:session-1': { savedAt: 4_000, items: [{ id: 'a', text: 'from before' }] } })
  on('session.id', () => ({ value: 'session-1' }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true } as never)

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, isWorking: false } })
  expect(await ui.find({ type: 'Text', text: /kept from before Claude restarted/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: 'from before' })).toBeDefined()
  await ui.unmount()
})

test('/clear carries the held prompts over to the new conversation, paused', async ($, on) => {
  let current = 'session-1'
  const clock = mock.clock(on, { now: 1_000 })
  mock.store(on)
  on('session.id', () => ({ value: current }))
  on('prompt.submit', (_, e) => ({ text: e.text }))
  on('session.end', (_, e) => ({ sessionId: e.sessionId }))

  await $.prompt.submit({ text: 'carry me', ...typedWhileBusy })
  await $.session.end({ reason: 'clear', sessionId: 'session-1' } as never)
  current = 'session-2'
  await clock.advance(1)

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, isWorking: false } })
  expect((await ui.findAll({ type: 'Text', text: 'carry me' })).length).toBe(1)
  expect(await ui.find({ type: 'Text', text: /kept from before \/clear/ })).toBeDefined()
  await ui.unmount()
})

test('Send now stops the running turn and sends the first held prompt', async ($, on) => {
  const entered = engine(on)
  const aborted: string[] = []
  on('turn.abort', (_, e) => {
    aborted.push(e.turnId)

    return { value: undefined }
  })
  await $.turn.start({ text: 'working', turnId: 't1' })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const sendNow = await ui.find({ type: 'Button', text: 'Send now' })
  expect(sendNow).toBeDefined()
  await ui.press({ key: sendNow?.key ?? '' })
  await ui.unmount()
  expect(aborted).toEqual(['t1'])
  expect(entered).toEqual([])

  await $.turn.complete({ turnId: 't1', reason: 'aborted', isAborted: true, answer: '', durationMs: 1, usage: {} } as never)
  expect(entered).toEqual(['first'])
})
