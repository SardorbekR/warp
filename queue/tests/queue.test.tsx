import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { TestBody } from 'claude-code/testing'

type Engine = Parameters<TestBody>[0]

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

// Cmd+Enter (chat:queueSubmit) while Claude works: the prompt the plugin holds.
const typedWhileBusy = { turnId: 't1', wait: true, origin: { kind: 'composer' } } as const

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

test('Cmd+Enter while Claude works is held; Enter and idle prompts go through', async ($, on) => {
  const entered = engine(on)

  expect(await $.prompt.submit({ text: 'later', ...typedWhileBusy })).toEqual({ drop: 'Queued' })
  await $.prompt.submit({ text: 'now', turnId: 't1', wait: false, origin: { kind: 'composer' } })
  await $.prompt.submit({ text: 'idle', wait: true, origin: { kind: 'composer' } })
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

// A gate the test opens: `reached` resolves once the stand-in waits at it.
function gate() {
  let open = () => {}
  let arrive = () => {}
  const opened = new Promise<void>(resolve => {
    open = resolve
  })
  const reached = new Promise<void>(resolve => {
    arrive = resolve
  })
  return { open, opened, reached, arrive }
}

// Stands for the model and the tools: a step or a tool call waits at its gate
// until the test opens it. `step` and `tool` resolve once it is waiting, with
// `finish` to let it end.
function loop(on: On) {
  let stepGate = gate()
  let stepTools = false
  let toolGate = gate()
  on('turn.step', async function* (_, e) {
    stepGate.arrive()
    await stepGate.opened
    const tools = stepTools

    return {
      turnId: e.turnId,
      index: e.index,
      answer: tools ? '' : 'done',
      toolUses: tools ? [{ name: 'Bash', input: { command: 'ls' } }] : [],
      stopReason: tools ? ('tool_use' as const) : ('end_turn' as const),
      usage: null,
    }
  })
  on('tool.call', async () => {
    toolGate.arrive()
    await toolGate.opened

    return { result: {} } as never
  })
  const drain = async (stream: AsyncIterable<unknown>) => {
    for await (const _ of stream) {
      // The stand-in streams nothing.
    }
  }
  return {
    step: async ($: Engine, turnId: string, tools: boolean) => {
      stepGate = gate()
      stepTools = tools
      const done = drain($.turn.step({ turnId, index: 0, model: 'm', messageCount: 1 }))
      await stepGate.reached

      return {
        finish: async () => {
          stepGate.open()
          await done
        },
      }
    },
    tool: async ($: Engine) => {
      toolGate = gate()
      const done = $.tool.call({ tool: 'Bash', command: 'ls' })
      await toolGate.reached

      return {
        finish: async () => {
          toolGate.open()
          await done
        },
      }
    },
  }
}

const answered = { reason: 'answer', isAborted: false, answer: '', durationMs: 1, usage: {} } as const

async function pressSendNow($: Engine) {
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const sendNow = await ui.find({ type: 'Button', text: 'Send now' })
  expect(sendNow).toBeDefined()
  await ui.press({ key: sendNow?.key ?? '' })
  const texts = (await ui.findAll({ type: 'Text' })).map(one => one.text)
  await ui.unmount()
  return texts
}

test('Send now while a tool runs gives Claude the prompt as a note, once, without stopping it', async ($, on) => {
  const entered = engine(on)
  const session = mock.session(on)
  const { step, tool } = loop(on)
  const aborted: string[] = []
  on('turn.abort', (_, e) => {
    aborted.push(e.turnId)

    return { value: undefined }
  })
  await $.turn.start({ text: 'working', turnId: 't1' })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })
  await (await step($, 't1', true)).finish()

  const running = await tool($)
  const texts = await pressSendNow($)
  expect(texts.includes('first')).toBe(false)
  await running.finish()
  expect(aborted).toEqual([])
  expect(entered).toEqual([])
  const rows = session.appended()
  expect(rows.length).toBe(1)
  expect(JSON.stringify(rows[0]?.message)).toMatch(/they just chose to send it now:\\n\\nfirst/)

  // The turn ends: the next held prompt goes, and the note is not sent again.
  await $.turn.complete({ turnId: 't1', ...answered } as never)
  expect(entered).toEqual(['second'])
  expect(session.appended().length).toBe(1)
})

test('Send now while Claude writes its last response sends the prompt as the person, once', async ($, on) => {
  const entered = engine(on)
  const session = mock.session(on)
  const { step } = loop(on)
  await $.turn.start({ text: 'working', turnId: 't1' })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })

  const writing = await step($, 't1', false)
  const texts = await pressSendNow($)
  expect(texts.includes('sends after this step')).toBe(true)
  await writing.finish()
  expect(session.appended().length).toBe(0)

  await $.turn.complete({ turnId: 't1', ...answered } as never)
  expect(entered).toEqual(['first'])
  expect(session.appended().length).toBe(0)
})

test('Send now before the first response, or while Claude writes one that asks for tools, adds the note when it ends', async ($, on) => {
  const entered = engine(on)
  const session = mock.session(on)
  const { step } = loop(on)
  await $.turn.start({ text: 'working', turnId: 't1' })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })

  expect((await $.command.run({ command: 'send-now', args: '' } as never)).text).toMatch(/when its current step ends.*: first$/)
  const writing = await step($, 't1', true)
  await pressSendNow($)
  expect(session.appended().length).toBe(0)
  await writing.finish()
  const rows = session.appended()
  expect(rows.length).toBe(2)
  expect(JSON.stringify(rows[0]?.message)).toMatch(/\\n\\nfirst/)
  expect(JSON.stringify(rows[1]?.message)).toMatch(/\\n\\nsecond/)

  await $.turn.complete({ turnId: 't1', ...answered } as never)
  expect(entered).toEqual([])
  expect(session.appended().length).toBe(2)
})

test('two prompts sent during the last response: the first starts the next turn, the second goes at its first tool', async ($, on) => {
  const entered = engine(on)
  const session = mock.session(on)
  const { step, tool } = loop(on)
  await $.turn.start({ text: 'working', turnId: 't1' })
  for (const text of ['first', 'second', 'third']) {
    await $.prompt.submit({ text, ...typedWhileBusy })
  }

  const writing = await step($, 't1', false)
  expect((await $.command.run({ command: 'send-now', args: '2' } as never)).text).toMatch(/when its current step ends.*: second$/)
  await pressSendNow($)
  await writing.finish()
  await $.turn.complete({ turnId: 't1', ...answered } as never)
  expect(entered).toEqual(['second'])
  expect(session.appended().length).toBe(0)

  // The next turn's first request is already put together: nothing goes in
  // until a tool runs.
  await $.turn.start({ text: 'second', turnId: 't2' })
  expect(session.appended().length).toBe(0)
  const running = await tool($)
  const rows = session.appended()
  expect(rows.length).toBe(1)
  expect(JSON.stringify(rows[0]?.message)).toMatch(/\\n\\nfirst/)
  await running.finish()
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const left = (await ui.findAll({ type: 'Text' })).map(one => one.text).filter(text => /^(first|second|third)$/.test(text))
  await ui.unmount()
  expect(left).toEqual(['third'])
})

test('Esc before the response ends keeps a prompt sent with Send now held', async ($, on) => {
  const entered = engine(on)
  const session = mock.session(on)
  const { step } = loop(on)
  await $.turn.start({ text: 'working', turnId: 't1' })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })

  const writing = await step($, 't1', false)
  await pressSendNow($)
  await writing.finish()
  await $.turn.complete({ turnId: 't1', reason: 'aborted', isAborted: true, answer: '', durationMs: 1, usage: {} } as never)
  expect(entered).toEqual([])
  expect(session.appended().length).toBe(0)

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, isWorking: false } })
  const texts = (await ui.findAll({ type: 'Text' })).map(one => one.text)
  await ui.unmount()
  expect(texts.includes('first')).toBe(true)
  expect(texts.some(text => /you stopped Claude/.test(text))).toBe(true)
  expect(texts.includes('sends after this step')).toBe(false)
})

test('an edited prompt goes back to its place when held again', async ($, on) => {
  const entered = engine(on)
  const filled: string[] = []
  on('prompt.read', () => ({ value: { text: '', cursor: 0 } }))
  on('prompt.fill', (_, e) => {
    filled.push(e.text)

    return { isFilled: true, text: e.text, cursor: e.text.length }
  })
  for (const text of ['first', 'second', 'third']) {
    await $.prompt.submit({ text, ...typedWhileBusy })
  }

  const editRow = async (row: number) => {
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    const edit = (await ui.findAll({ type: 'Button', text: 'Edit' }))[row]
    expect(edit).toBeDefined()
    await ui.press({ key: edit?.key ?? '' })
    await ui.unmount()
  }
  const prompts = new Set(['first', 'first fixed', 'second', 'second fixed', 'third', 'idle'])
  const rows = async () => {
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    const texts = (await ui.findAll({ type: 'Text' })).map(one => one.text)
    await ui.unmount()
    return texts.filter(text => prompts.has(text))
  }

  await editRow(0)
  expect(filled).toEqual(['first'])
  await $.prompt.submit({ text: 'first fixed', ...typedWhileBusy })
  expect(await rows()).toEqual(['first fixed', 'second', 'third'])

  // The prompt before it is sent while it is being edited: it goes first.
  await editRow(1)
  await $.turn.complete({ turnId: 't1', reason: 'answer', isAborted: false, answer: '', durationMs: 1, usage: {} } as never)
  expect(entered).toEqual(['first fixed'])
  await $.prompt.submit({ text: 'second fixed', ...typedWhileBusy })
  expect(await rows()).toEqual(['second fixed', 'third'])

  // Any other submit from the box forgets the place: a new prompt goes last.
  await editRow(0)
  await $.prompt.submit({ text: 'idle', wait: false, origin: { kind: 'composer' } })
  await $.prompt.submit({ text: 'first fixed', ...typedWhileBusy })
  expect(await rows()).toEqual(['third', 'first fixed'])
})

test('/send-now gives Claude the held prompt it names, without stopping it', async ($, on) => {
  engine(on)
  const session = mock.session(on)
  const { tool } = loop(on)
  const aborted: string[] = []
  on('turn.abort', (_, e) => {
    aborted.push(e.turnId)

    return { value: undefined }
  })
  await $.turn.start({ text: 'working', turnId: 't1' })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })

  const running = await tool($)
  expect((await $.command.run({ command: 'send-now', args: '3' } as never)).text).toBe('There is no held prompt 3. 2 are queued.')
  expect((await $.command.run({ command: 'send-now', args: '2' } as never)).text).toMatch(/without stopping it.*: second$/)
  await running.finish()
  expect(aborted).toEqual([])
  expect(JSON.stringify(session.appended()[0]?.message)).toMatch(/\\n\\nsecond/)

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const texts = (await ui.findAll({ type: 'Text' })).map(one => one.text).filter(text => /^(first|second)$/.test(text))
  await ui.unmount()
  expect(texts).toEqual(['first'])
})

test('/edit-queued puts a held prompt back in the box', async ($, on) => {
  engine(on)
  const filled: string[] = []
  on('prompt.read', () => ({ value: { text: '', cursor: 0 } }))
  on('prompt.fill', (_, e) => {
    filled.push(e.text)

    return { isFilled: true, text: e.text, cursor: e.text.length }
  })
  await $.prompt.submit({ text: 'first', ...typedWhileBusy })
  await $.prompt.submit({ text: 'second', ...typedWhileBusy })

  expect((await $.command.run({ command: 'edit-queued', args: '2' } as never)).text).toMatch(/Back in the prompt box/)
  expect(filled).toEqual(['second'])
  await $.prompt.submit({ text: 'second fixed', ...typedWhileBusy })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const texts = (await ui.findAll({ type: 'Text' })).map(one => one.text).filter(text => /^(first|second fixed)$/.test(text))
  await ui.unmount()
  expect(texts).toEqual(['first', 'second fixed'])
})
