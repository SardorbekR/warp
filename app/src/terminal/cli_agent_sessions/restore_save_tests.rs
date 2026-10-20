use std::cell::Cell;
use std::rc::Rc;

use warpui::{App, ModelHandle, WindowId};

use super::*;

/// Counts `workspace:save_app` dispatches.
fn count_saves(app: &mut App) -> Rc<Cell<usize>> {
    let saves = Rc::new(Cell::new(0_usize));
    let counter = saves.clone();
    app.add_global_action(SAVE_APP_ACTION, move |_: &(), _| {
        counter.set(counter.get().saturating_add(1));
    });
    saves
}

fn scheduler(
    app: &mut App,
    quiet_period_ms: u64,
    max_delay_ms: u64,
    retry_delay_ms: u64,
) -> ModelHandle<CLIAgentRestoreSaveScheduler> {
    scheduler_with_lazy_delay(app, quiet_period_ms, max_delay_ms, retry_delay_ms, 60_000)
}

fn scheduler_with_lazy_delay(
    app: &mut App,
    quiet_period_ms: u64,
    max_delay_ms: u64,
    retry_delay_ms: u64,
    lazy_delay_ms: u64,
) -> ModelHandle<CLIAgentRestoreSaveScheduler> {
    app.add_singleton_model(|_| {
        CLIAgentRestoreSaveScheduler::with_delays(
            Duration::from_millis(quiet_period_ms),
            Duration::from_millis(max_delay_ms),
            Duration::from_millis(retry_delay_ms),
            Duration::from_millis(lazy_delay_ms),
        )
    })
}

fn request_save(app: &mut App, scheduler: &ModelHandle<CLIAgentRestoreSaveScheduler>) {
    scheduler.update(app, |scheduler, ctx| scheduler.request_save(ctx));
}

fn request_lazy_save(app: &mut App, scheduler: &ModelHandle<CLIAgentRestoreSaveScheduler>) {
    scheduler.update(app, |scheduler, ctx| scheduler.request_lazy_save(ctx));
}

fn is_dirty(app: &App, scheduler: &ModelHandle<CLIAgentRestoreSaveScheduler>) -> bool {
    scheduler.read(app, |scheduler, _| scheduler.dirty_since.is_some())
}

async fn wait_ms(ms: u64) {
    Timer::after(Duration::from_millis(ms)).await;
}

#[test]
fn requests_are_coalesced_into_one_save() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let scheduler = scheduler(&mut app, 30, 1_000, 20);
        for _ in 0..5 {
            request_save(&mut app, &scheduler);
        }
        assert_eq!(saves.get(), 0, "saves wait for the quiet period");

        wait_ms(150).await;
        assert_eq!(saves.get(), 1);
        assert!(!is_dirty(&app, &scheduler));
    })
}

#[test]
fn continuous_changes_are_saved_within_the_max_delay() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        // Changes arrive faster than the quiet period, so only the cap can trigger a save.
        let scheduler = scheduler(&mut app, 60, 120, 20);
        for _ in 0..15 {
            request_save(&mut app, &scheduler);
            wait_ms(20).await;
        }
        assert!(
            saves.get() >= 1,
            "300ms of continuous changes must be saved despite the 60ms quiet period"
        );

        wait_ms(200).await;
        assert!(!is_dirty(&app, &scheduler));
    })
}

#[test]
fn save_skipped_during_a_tab_drag_is_retried() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let drag = app.add_singleton_model(|_| CrossWindowTabDrag::new());
        let window_id = WindowId::new();
        drag.update(&mut app, |drag, _| {
            drag.register_pending_source_close(window_id)
        });
        let scheduler = scheduler(&mut app, 10, 1_000, 20);

        request_save(&mut app, &scheduler);
        wait_ms(100).await;
        assert_eq!(saves.get(), 0, "`workspace:save_app` would skip this save");
        assert!(is_dirty(&app, &scheduler), "the change stays pending");

        drag.update(&mut app, |drag, _| {
            drag.finish_pending_source_close(window_id)
        });
        wait_ms(100).await;
        assert_eq!(saves.get(), 1);
        assert!(!is_dirty(&app, &scheduler));
    })
}

#[test]
fn flush_saves_a_pending_change_right_away() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let scheduler = scheduler(&mut app, 60_000, 60_000, 60_000);

        app.update(flush_pending_save);
        assert_eq!(saves.get(), 0, "nothing to flush");

        request_save(&mut app, &scheduler);
        app.update(flush_pending_save);
        assert_eq!(saves.get(), 1, "the shutdown flush saves synchronously");
        assert!(!is_dirty(&app, &scheduler));

        // The debounced save was cancelled, so it doesn't save a second time.
        app.update(flush_pending_save);
        assert_eq!(saves.get(), 1);
    })
}

#[test]
fn lazy_requests_wait_for_the_lazy_delay() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let scheduler = scheduler_with_lazy_delay(&mut app, 10, 1_000, 20, 150);
        for _ in 0..5 {
            request_lazy_save(&mut app, &scheduler);
        }
        wait_ms(60).await;
        assert_eq!(saves.get(), 0, "a lazy change ignores the quiet period");
        assert!(is_dirty(&app, &scheduler));

        wait_ms(250).await;
        assert_eq!(saves.get(), 1);
        assert!(!is_dirty(&app, &scheduler));
    })
}

#[test]
fn a_regular_request_saves_a_lazy_change_sooner() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let scheduler = scheduler_with_lazy_delay(&mut app, 20, 1_000, 20, 60_000);
        request_lazy_save(&mut app, &scheduler);
        request_save(&mut app, &scheduler);

        wait_ms(100).await;
        assert_eq!(saves.get(), 1);
        assert!(!is_dirty(&app, &scheduler));

        // The lazy save was part of that one.
        wait_ms(100).await;
        assert_eq!(saves.get(), 1);
    })
}

#[test]
fn lazy_requests_never_delay_a_pending_save() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let scheduler = scheduler_with_lazy_delay(&mut app, 40, 1_000, 20, 60_000);
        request_save(&mut app, &scheduler);
        request_lazy_save(&mut app, &scheduler);

        wait_ms(150).await;
        assert_eq!(saves.get(), 1);
        assert!(!is_dirty(&app, &scheduler));
    })
}

#[test]
fn flush_saves_a_pending_lazy_change() {
    App::test((), |mut app| async move {
        let saves = count_saves(&mut app);
        let scheduler = scheduler_with_lazy_delay(&mut app, 60_000, 60_000, 60_000, 60_000);
        request_lazy_save(&mut app, &scheduler);

        app.update(flush_pending_save);
        assert_eq!(saves.get(), 1);
        assert!(!is_dirty(&app, &scheduler));
    })
}
