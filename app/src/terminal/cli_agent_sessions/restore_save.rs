//! Coalesces the app-state saves requested because a pane's CLI agent session record changed.
//!
//! Building the app-state snapshot walks every window, so agent activity across many panes
//! must not trigger one save per event. Requests are debounced, with a cap on how long a change
//! can wait, and a save that would be skipped (e.g. during a cross-window tab drag) is retried
//! instead of being forgotten.

use std::time::Duration;

use instant::Instant;
use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::workspace::cross_window_tab_drag::CrossWindowTabDrag;

/// How long to wait for further changes before saving.
const SAVE_QUIET_PERIOD: Duration = Duration::from_millis(750);
/// The longest a change waits to be saved while changes keep arriving.
const SAVE_MAX_DELAY: Duration = Duration::from_secs(3);
/// How long to wait before retrying a save that couldn't run.
const SAVE_RETRY_DELAY: Duration = Duration::from_secs(1);

const SAVE_APP_ACTION: &str = "workspace:save_app";

#[derive(Default)]
pub struct CLIAgentRestoreSaveScheduler {
    /// When the oldest unsaved change was requested, if any change is unsaved.
    dirty_since: Option<Instant>,
    timer: Option<SpawnedFutureHandle>,
}

impl Entity for CLIAgentRestoreSaveScheduler {
    type Event = ();
}

impl SingletonEntity for CLIAgentRestoreSaveScheduler {}

impl CLIAgentRestoreSaveScheduler {
    /// Schedules a save of the app state.
    pub fn request_save(&mut self, ctx: &mut ModelContext<Self>) {
        let now = Instant::now();
        let dirty_since = *self.dirty_since.get_or_insert(now);
        let waited = now.saturating_duration_since(dirty_since);
        let delay = SAVE_QUIET_PERIOD.min(SAVE_MAX_DELAY.saturating_sub(waited));
        self.schedule(delay, ctx);
    }

    fn schedule(&mut self, delay: Duration, ctx: &mut ModelContext<Self>) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
        self.timer = Some(ctx.spawn(
            async move {
                Timer::after(delay).await;
            },
            |me, _, ctx| {
                me.timer = None;
                me.save_if_dirty(ctx);
            },
        ));
    }

    fn save_if_dirty(&mut self, ctx: &mut ModelContext<Self>) {
        if self.dirty_since.is_none() {
            return;
        }
        if is_save_deferred(ctx) {
            // `workspace:save_app` would silently skip this save; keep the change pending.
            self.schedule(SAVE_RETRY_DELAY, ctx);
            return;
        }
        self.dirty_since = None;
        ctx.dispatch_global_action(SAVE_APP_ACTION, ());
    }

    /// Clears the pending save, returning whether there was one.
    fn take_pending(&mut self) -> bool {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
        self.dirty_since.take().is_some()
    }
}

/// Whether `workspace:save_app` currently skips saving for a transient reason.
fn is_save_deferred(ctx: &AppContext) -> bool {
    ctx.has_singleton_model::<CrossWindowTabDrag>() && CrossWindowTabDrag::as_ref(ctx).is_active()
}

pub(crate) fn init(ctx: &mut AppContext) {
    if !ctx.has_singleton_model::<CLIAgentRestoreSaveScheduler>() {
        ctx.add_singleton_model(|_| CLIAgentRestoreSaveScheduler::default());
    }
}

/// Saves the app state right away if a debounced save is pending. Call this before the
/// persistence writer shuts down so a change made just before quitting isn't lost.
pub fn flush_pending_save(ctx: &mut AppContext) {
    if !ctx.has_singleton_model::<CLIAgentRestoreSaveScheduler>() {
        return;
    }
    let had_pending = CLIAgentRestoreSaveScheduler::handle(ctx)
        .update(ctx, |scheduler, _| scheduler.take_pending());
    if had_pending {
        ctx.dispatch_global_action(SAVE_APP_ACTION, &());
    }
}
