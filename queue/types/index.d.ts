export type QueuedPrompt = { id: string; text: string }

// Why the queue waits for a press instead of sending at the next turn end.
export type QueuePause = 'stopped' | 'error' | 'restored' | 'resumed' | 'cleared'

declare module 'claude-code' {
  interface PluginState {
    queue: { items: Shaped<QueuedPrompt[]>; paused: QueuePause | null; marked: string[] }
  }
}
