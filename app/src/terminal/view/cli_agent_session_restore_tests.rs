use std::cell::Cell;
use std::rc::Rc;

use warpui::{App, EntityId, ViewHandle};

use super::*;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::event::{
    CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType,
};
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
};
use crate::test_util::terminal::{add_window_with_terminal, initialize_app_for_terminal_view};

const SESSION_ID: &str = "3f2a9c1e-8d4b-4f6a-9e2d-1b7c5a0f8e3d";
const OTHER_SESSION_ID: &str = "9b1d2c3e-4f5a-4b6c-8d7e-0f1a2b3c4d5e";
const MISSING_LAUNCH_DIRECTORY: &str = "/nonexistent/warp cli agent restore tests";

/// A terminal whose login shell hasn't bootstrapped yet, as `restore_pane_leaf` sees it.
fn terminal(app: &mut App) -> ViewHandle<TerminalView> {
    initialize_app_for_terminal_view(app);
    add_window_with_terminal(app, None)
}

/// A terminal whose login shell has bootstrapped, so the banner is shown right away.
async fn bootstrapped_terminal(app: &mut App) -> ViewHandle<TerminalView> {
    let terminal = terminal(app);
    wait_for_bootstrap(app, &terminal).await;
    terminal
}

/// Waits for the login shell of `terminal` to bootstrap, which also saves the app state.
async fn wait_for_bootstrap(app: &App, terminal: &ViewHandle<TerminalView>) {
    wait_for(app, |app| {
        terminal.read(app, |view, _| {
            view.cli_agent_session_restore.shell_ready_handled
        })
    })
    .await;
    // Let the rest of the bootstrap sequence land.
    process_terminal_events().await;
    terminal.read(app, |view, _| {
        assert!(view.cli_agent_session_restore.shell_ready_handled);
    });
}

/// Counts `workspace:save_app` dispatches. No save scheduler is registered in these tests, so
/// each record change dispatches one directly.
fn count_saves(app: &mut App) -> Rc<Cell<usize>> {
    let saves = Rc::new(Cell::new(0_usize));
    let counter = saves.clone();
    app.add_global_action("workspace:save_app", move |_: &(), _| {
        counter.set(counter.get().saturating_add(1));
    });
    saves
}

/// Counts `Event::TerminalViewStateChanged`, which tells the workspace (and its sidebar) to
/// re-read the pane.
fn count_state_changes(app: &mut App, terminal: &ViewHandle<TerminalView>) -> Rc<Cell<usize>> {
    let changes = Rc::new(Cell::new(0_usize));
    let counter = changes.clone();
    app.update(|ctx| {
        ctx.subscribe_to_view(terminal, move |_, event, _| {
            if matches!(event, Event::TerminalViewStateChanged) {
                counter.set(counter.get().saturating_add(1));
            }
        });
    });
    changes
}

/// A launch directory that exists on this machine, with a space so it has to be quoted.
fn launch_dir() -> String {
    let dir = std::env::temp_dir().join("warp cli agent restore tests");
    std::fs::create_dir_all(&dir).expect("the test launch directory can be created");
    dir.to_string_lossy().into_owned()
}

fn cd_command() -> String {
    format!("cd '{}'", launch_dir())
}

fn restored_record() -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        agent: CLIAgent::Claude,
        session_id: Some(SESSION_ID.to_owned()),
        launch_cwd: Some(launch_dir()),
        title: Some("fix the login bug".to_owned()),
        transcript_path: None,
        status: None,
        remote_host: None,
        launched_locally: true,
        evidently_interactive: true,
        updated_at_unix_ms: 1_000,
        last_activity_at_unix_ms: None,
    }
}

/// A session that ran in a directory this machine doesn't have, e.g. on another computer.
fn record_from_elsewhere() -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        launch_cwd: Some(MISSING_LAUNCH_DIRECTORY.to_owned()),
        ..restored_record()
    }
}

/// A live session as command detection or the plugin registers it.
fn live_session(
    agent: CLIAgent,
    received_rich_notification: bool,
    session_id: Option<&str>,
    query: Option<&str>,
) -> CLIAgentSession {
    CLIAgentSession {
        agent,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext {
            session_id: session_id.map(str::to_owned),
            resume_session_id: session_id.map(str::to_owned),
            query: query.map(str::to_owned),
            ..Default::default()
        },
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification,
    }
}

fn start_live_session(app: &mut App, view_id: EntityId, session: CLIAgentSession) {
    CLIAgentSessionsModel::handle(app).update(app, |sessions, ctx| {
        sessions.set_session(view_id, session, ctx);
    });
}

fn end_live_session(app: &mut App, view_id: EntityId) {
    CLIAgentSessionsModel::handle(app).update(app, |sessions, ctx| {
        sessions.remove_session(view_id, ctx);
    });
}

/// Flips the live session's status (blocked on a permission request, then running again)
/// without changing anything the record keeps.
fn flip_status(app: &mut App, view_id: EntityId) {
    let event = |event| CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event,
        session_id: Some(SESSION_ID.to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    CLIAgentSessionsModel::handle(app).update(app, |sessions, ctx| {
        sessions.update_from_event(view_id, &event(CLIAgentEventType::PermissionRequest), ctx);
        sessions.update_from_event(view_id, &event(CLIAgentEventType::PermissionReplied), ctx);
    });
}

fn simulate_running_command(app: &mut App, terminal: &ViewHandle<TerminalView>, command: &str) {
    terminal.update(app, |view, _| {
        view.model.lock().simulate_long_running_block(command, "");
    });
}

fn complete_agent_block(app: &mut App, terminal: &ViewHandle<TerminalView>, succeeded: bool) {
    terminal.update(app, |view, ctx| {
        view.on_user_block_completed_for_cli_agent_restore(succeeded, ctx);
    });
}

fn restore(app: &mut App, terminal: &ViewHandle<TerminalView>, record: PersistedCLIAgentSession) {
    terminal.update(app, |view, ctx| view.restore_cli_agent_session(record, ctx));
}

fn resume(app: &mut App, terminal: &ViewHandle<TerminalView>) -> CLIAgentResumeOutcome {
    terminal.update(app, |view, ctx| view.resume_restored_cli_agent_session(ctx))
}

fn time_out(app: &mut App, terminal: &ViewHandle<TerminalView>) {
    terminal.update(app, |view, ctx| view.on_cli_agent_resume_timeout(ctx));
}

fn snapshot(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<PersistedCLIAgentSession> {
    terminal.read(app, |view, _| view.cli_agent_session_for_snapshot())
}

fn restorable(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<PersistedCLIAgentSession> {
    terminal.read(app, |view, _| view.restorable_cli_agent_session().cloned())
}

fn can_resume_here(app: &App, terminal: &ViewHandle<TerminalView>) -> bool {
    terminal.read(app, |view, _| {
        view.can_resume_restored_cli_agent_session_here()
    })
}

fn phase(app: &App, terminal: &ViewHandle<TerminalView>) -> RecoveryPhase {
    terminal.read(app, |view, _| view.cli_agent_session_restore.phase)
}

fn previous_resume(app: &App, terminal: &ViewHandle<TerminalView>) -> PreviousResumeAttempt {
    terminal.read(app, |view, _| {
        view.cli_agent_session_restore.previous_resume
    })
}

fn has_banner(app: &App, terminal: &ViewHandle<TerminalView>) -> bool {
    terminal.read(app, |view, _| {
        view.cli_agent_session_restore.banner.is_some()
    })
}

/// The banner's view id, if it's shown in the block list.
fn banner_in_block_list(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<EntityId> {
    terminal.read(app, |view, _| {
        let banner_id = view.cli_agent_session_restore.banner.as_ref()?.id();
        view.rich_content_views
            .iter()
            .any(|rich_content| rich_content.view_id() == banner_id)
            .then_some(banner_id)
    })
}

fn has_resume_timer(app: &App, terminal: &ViewHandle<TerminalView>) -> bool {
    terminal.read(app, |view, _| {
        view.cli_agent_session_restore.resume_timer.is_some()
    })
}

fn in_flight_step(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<ResumeStep> {
    terminal.read(app, |view, _| {
        view.cli_agent_session_restore
            .in_flight_resume_step
            .as_ref()
            .map(|(step, _)| *step)
    })
}

fn type_draft(app: &mut App, terminal: &ViewHandle<TerminalView>) {
    terminal.update(app, |view, ctx| {
        view.input.update(ctx, |input, ctx| {
            input.replace_buffer_content("half-typed command", ctx);
        });
    });
}

fn input_text(app: &App, terminal: &ViewHandle<TerminalView>) -> String {
    terminal.read(app, |view, ctx| view.input.as_ref(ctx).buffer_text(ctx))
}

fn input_has_pending_command(app: &App, terminal: &ViewHandle<TerminalView>) -> bool {
    terminal.read(app, |view, ctx| {
        view.input.as_ref(ctx).has_pending_command()
    })
}

/// Whether `command` was handed to the shell: it's either running (the pending command was
/// executed) or waiting in the input for the shell to accept it.
fn command_was_sent(app: &App, terminal: &ViewHandle<TerminalView>, command: &str) -> bool {
    terminal.read(app, |view, ctx| {
        view.awaiting_pending_command_completion
            || view.input.as_ref(ctx).buffer_text(ctx) == command
    })
}

/// Lets the terminal process the events its model has queued and the timers that follow. The
/// test terminal's model runs the real bootstrap sequence (`InitShell`, `Bootstrapped`, then
/// `CommandFinished` and `Precmd`) when it's created, and the view only receives those events
/// once the test yields to the executor.
async fn process_terminal_events() {
    Timer::after(Duration::from_millis(250)).await;
}

/// Yields to the executor until `condition` holds, for at most a few seconds.
async fn wait_for(app: &App, condition: impl Fn(&App) -> bool) {
    for _ in 0..60 {
        if condition(app) {
            return;
        }
        Timer::after(Duration::from_millis(50)).await;
    }
}

/// Puts the pane in the state right after the resume commands were sent to the shell.
fn mark_resume_sent(app: &mut App, terminal: &ViewHandle<TerminalView>) {
    terminal.update(app, |view, _| {
        view.cli_agent_session_restore.phase = RecoveryPhase::ResumePending;
    });
}

/// Puts the pane in the state where the shell runs `step` of a resume, with `remaining` still
/// to be typed.
fn mark_resume_step_running(
    app: &mut App,
    terminal: &ViewHandle<TerminalView>,
    step: ResumeStep,
    remaining: &[(ResumeStep, &str)],
) {
    terminal.update(app, |view, _| {
        let state = &mut view.cli_agent_session_restore;
        state.phase = RecoveryPhase::ResumePending;
        let command = match step {
            ResumeStep::ChangeDirectory => cd_command(),
            ResumeStep::Agent => agent_command(),
        };
        state.in_flight_resume_step = Some((step, command));
        state.pending_resume_steps = remaining
            .iter()
            .map(|(step, command)| (*step, (*command).to_owned()))
            .collect();
        view.awaiting_pending_command_completion = true;
    });
}

fn agent_command() -> String {
    format!("claude --resume {SESSION_ID}")
}

/// The record of an agent session that reports through a pane running `command`.
async fn record_of_agent_reporting_from(app: &mut App, command: &str) -> PersistedCLIAgentSession {
    let terminal = bootstrapped_terminal(app).await;
    simulate_running_command(app, &terminal, command);
    start_live_session(
        app,
        terminal.id(),
        live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
    );
    snapshot(app, &terminal).expect("the live session is persisted")
}

#[test]
fn restored_session_shows_banner_after_real_bootstrap() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        // `restore_pane_leaf` hands the record over before the shell has bootstrapped.
        terminal.read(&app, |view, _| assert!(!view.is_login_shell_bootstrapped));
        restore(&mut app, &terminal, restored_record());
        assert!(!has_banner(&app, &terminal));

        wait_for(&app, |app| has_banner(app, &terminal)).await;

        terminal.read(&app, |view, _| assert!(view.is_login_shell_bootstrapped));
        assert!(banner_in_block_list(&app, &terminal).is_some());
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(can_resume_here(&app, &terminal));
    })
}

#[test]
fn restored_session_keeps_its_last_activity_for_the_sidebar() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        let saved_at = chrono::Utc::now().timestamp_millis() - 2 * 60 * 60 * 1000;
        let mut record = restored_record();
        record.last_activity_at_unix_ms = Some(saved_at);
        restore(&mut app, &terminal, record);

        AgentNotificationsModel::handle(&app).read(&app, |model, _| {
            assert_eq!(model.last_activity_unix_ms(view_id), Some(saved_at));
        });
        // Saving the app state again keeps the same time.
        let saved = terminal.read(&app, |view, ctx| view.cli_agent_session_for_app_state(ctx));
        assert_eq!(
            saved.and_then(|record| record.last_activity_at_unix_ms),
            Some(saved_at)
        );
    })
}

#[test]
fn queued_resume_is_sent_after_real_bootstrap() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        // e.g. "Resume all" right after startup, before the shell is ready.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);
        terminal.read(&app, |view, _| {
            assert!(
                view.cli_agent_session_restore
                    .bootstrap_fallback_deadline
                    .is_some(),
                "only the long fallback runs until the shell has bootstrapped"
            );
        });

        wait_for(&app, |app| {
            phase(app, &terminal) == RecoveryPhase::ResumePending
        })
        .await;

        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        // The pane isn't in the launch directory, so it `cd`s there first.
        assert_eq!(
            in_flight_step(&app, &terminal),
            Some(ResumeStep::ChangeDirectory)
        );
        assert!(command_was_sent(&app, &terminal, &cd_command()));
        terminal.read(&app, |view, _| {
            let remaining: Vec<_> = view
                .cli_agent_session_restore
                .pending_resume_steps
                .iter()
                .cloned()
                .collect();
            assert_eq!(remaining, vec![(ResumeStep::Agent, agent_command())]);
        });
        assert!(!has_banner(&app, &terminal));
        // A crash now would still restore the session.
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn queued_resume_falls_back_to_the_banner_when_input_has_a_draft() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        type_draft(&mut app, &terminal);

        wait_for(&app, |app| has_banner(app, &terminal)).await;

        assert_eq!(input_text(&app, &terminal), "half-typed command");
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert!(has_banner(&app, &terminal));
    })
}

#[test]
fn queued_resume_does_not_time_out_during_bootstrap() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);

        // e.g. a timer armed by an earlier resume fires while the shell still bootstraps.
        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);
        assert!(!has_banner(&app, &terminal));
        assert_eq!(restorable(&app, &terminal), None);

        wait_for(&app, |app| {
            phase(app, &terminal) == RecoveryPhase::ResumePending
        })
        .await;
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        assert!(command_was_sent(&app, &terminal, &cd_command()));
        assert!(!has_banner(&app, &terminal));
    })
}

#[test]
fn resume_during_bootstrap_is_queued_even_if_the_pane_looks_busy() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        // The bootstrap (or a startup command) runs before the shell is ready.
        simulate_running_command(&mut app, &terminal, "source ~/.zshrc");

        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn queued_resume_waits_while_busy_and_times_out_after_bootstrap() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        // The resume waits for the launch directory check.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert!(has_resume_timer(&app, &terminal));
        // Meanwhile something starts running in the pane.
        simulate_running_command(&mut app, &terminal, "vim");

        // The launch directory is found, but the pane stays busy.
        process_terminal_events().await;
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);
        assert_eq!(in_flight_step(&app, &terminal), None);
        assert!(!has_banner(&app, &terminal));

        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(has_banner(&app, &terminal));
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Failed
        );
    })
}

#[test]
fn restored_record_is_kept_until_dismissed() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let saves = count_saves(&mut app);
        restore(&mut app, &terminal, restored_record());

        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));

        terminal.update(&mut app, |view, ctx| {
            view.dismiss_restored_cli_agent_session(ctx);
        });
        assert_eq!(snapshot(&app, &terminal), None);
        assert_eq!(restorable(&app, &terminal), None);
        assert_eq!(saves.get(), 1);
    })
}

#[test]
fn entering_and_leaving_interrupted_notifies_the_workspace() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        let changes = count_state_changes(&mut app, &terminal);

        restore(&mut app, &terminal, restored_record());
        assert_eq!(changes.get(), 1, "entered Interrupted");

        // Resuming leaves Interrupted; the timeout brings it back.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert_eq!(changes.get(), 2);
        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(changes.get(), 3);

        terminal.update(&mut app, |view, ctx| {
            view.dismiss_restored_cli_agent_session(ctx);
        });
        assert_eq!(changes.get(), 4, "left Interrupted");

        // Tracking a live session doesn't touch the Interrupted phase, so status flips stay
        // quiet.
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );
        flip_status(&mut app, view_id);
        assert_eq!(changes.get(), 4);
    })
}

/// Status flips don't save right away. (Without a save scheduler there is no lazy save; see
/// `status_flips_are_saved_lazily`.)
#[test]
fn status_flips_do_not_save() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        let saves = count_saves(&mut app);
        let mut session = live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it"));
        // Already seen to be interactive, so the permission requests below change nothing the
        // record keeps.
        session.session_context.showed_interactive_prompt = true;
        start_live_session(&mut app, view_id, session);
        assert_eq!(saves.get(), 1);

        for _ in 0..5 {
            flip_status(&mut app, view_id);
        }
        assert_eq!(saves.get(), 1);
    })
}

#[test]
fn status_flips_are_saved_lazily() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        let saves = count_saves(&mut app);
        app.add_singleton_model(|_| {
            CLIAgentRestoreSaveScheduler::with_delays(
                Duration::from_millis(10),
                Duration::from_millis(1_000),
                Duration::from_millis(10),
                Duration::from_millis(300),
            )
        });
        let mut session = live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it"));
        session.session_context.showed_interactive_prompt = true;
        start_live_session(&mut app, view_id, session);
        Timer::after(Duration::from_millis(100)).await;
        assert_eq!(saves.get(), 1, "the record is saved after the quiet period");

        // The saved app state keeps when the status last changed, so status changes are saved
        // too, but only lazily and together.
        for _ in 0..5 {
            flip_status(&mut app, view_id);
        }
        Timer::after(Duration::from_millis(100)).await;
        assert_eq!(saves.get(), 1);
        Timer::after(Duration::from_millis(400)).await;
        assert_eq!(saves.get(), 2);
    })
}

#[test]
fn unsupported_agents_are_not_restored() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let mut record = restored_record();
        record.agent = CLIAgent::Gemini;
        restore(&mut app, &terminal, record);
        assert_eq!(snapshot(&app, &terminal), None);
    })
}

#[test]
fn deliberate_exit_clears_the_live_record() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        let saves = count_saves(&mut app);

        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Codex, true, Some(SESSION_ID), Some("refactor")),
        );
        let live = snapshot(&app, &terminal).expect("the live Codex session is persisted");
        assert_eq!(live.agent, CLIAgent::Codex);
        assert_eq!(live.title.as_deref(), Some("refactor"));
        assert_eq!(saves.get(), 1);

        // The session ending alone isn't a deliberate exit.
        end_live_session(&mut app, view_id);
        assert_eq!(snapshot(&app, &terminal), Some(live));

        complete_agent_block(&mut app, &terminal, true);
        assert_eq!(snapshot(&app, &terminal), None);
        assert_eq!(saves.get(), 2);
    })
}

#[test]
fn failed_agent_block_leaves_the_session_interrupted() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        simulate_running_command(&mut app, &terminal, "claude");
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );
        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, false);

        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        let restorable = restorable(&app, &terminal).expect("the session can be offered");
        assert_eq!(restorable.session_id.as_deref(), Some(SESSION_ID));
        assert!(has_banner(&app, &terminal));
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::NotAttempted
        );
    })
}

#[test]
fn fresh_session_is_not_persisted_until_it_has_activity() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), None),
        );
        assert_eq!(snapshot(&app, &terminal), None);
    })
}

#[test]
fn agent_started_from_the_local_command_line_is_resumable() {
    App::test((), |mut app| async move {
        let live = record_of_agent_reporting_from(&mut app, "claude --model opus").await;
        assert!(live.launched_locally);
        assert!(live.can_resume_here());
        assert!(live.evidently_interactive);
    })
}

#[test]
fn agent_started_by_a_shell_function_is_resumable() {
    App::test((), |mut app| async move {
        let live = record_of_agent_reporting_from(&mut app, "my_claude_function --fast").await;
        assert!(live.launched_locally);
    })
}

#[test]
fn agent_started_after_cd_is_resumable() {
    App::test((), |mut app| async move {
        let live = record_of_agent_reporting_from(&mut app, "cd 'my repo' && claude").await;
        assert!(live.launched_locally);
    })
}

#[test]
fn agent_started_through_a_transparent_prefix_is_resumable() {
    App::test((), |mut app| async move {
        let live = record_of_agent_reporting_from(&mut app, "caffeinate -i npx claude").await;
        assert!(live.launched_locally);
    })
}

#[test]
fn agent_inside_tmux_is_copy_only() {
    App::test((), |mut app| async move {
        let live = record_of_agent_reporting_from(&mut app, "tmux new -s work").await;
        assert!(!live.launched_locally);
        assert!(!live.can_resume_here());
        assert!(!live.evidently_interactive);
    })
}

#[test]
fn agent_reported_from_another_command_is_copy_only() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();

        // e.g. the agent runs on the other side of an SSH session Warp didn't take over, and
        // its plugin reports through this pane.
        simulate_running_command(&mut app, &terminal, "ssh devbox");
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );
        // The user answered a permission prompt, so it's an interactive session.
        flip_status(&mut app, view_id);
        let live = snapshot(&app, &terminal).expect("the live session is persisted");
        assert!(!live.launched_locally);
        assert!(live.evidently_interactive);

        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(restorable(&app, &terminal), Some(live));
        assert!(!can_resume_here(&app, &terminal));
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::CopyOnly);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
    })
}

#[test]
fn session_whose_directory_is_missing_here_is_copy_only() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let changes = count_state_changes(&mut app, &terminal);
        restore(&mut app, &terminal, record_from_elsewhere());
        assert_eq!(changes.get(), 1);

        // The directory is checked off the main thread.
        wait_for(&app, |app| !can_resume_here(app, &terminal)).await;

        assert!(!can_resume_here(&app, &terminal));
        assert_eq!(changes.get(), 2, "the sidebar re-reads the pane");
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::CopyOnly);
        assert_eq!(restorable(&app, &terminal), Some(record_from_elsewhere()));
        assert!(has_banner(&app, &terminal));
    })
}

#[test]
fn queued_resume_into_a_missing_directory_is_offered_again() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, record_from_elsewhere());
        // Resumed before the directory check finished.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);

        wait_for(&app, |app| {
            phase(app, &terminal) == RecoveryPhase::Interrupted
        })
        .await;

        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(in_flight_step(&app, &terminal), None);
        assert_eq!(input_text(&app, &terminal), "");
        assert!(has_banner(&app, &terminal));
        assert!(!can_resume_here(&app, &terminal));
    })
}

#[test]
fn new_agent_session_with_activity_supersedes_restored_record() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        // The user starts the agent in the pane.
        simulate_running_command(&mut app, &terminal, "claude");

        // A fresh session that hasn't done anything leaves the restored record alone.
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(OTHER_SESSION_ID), None),
        );
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));

        // Once it has a prompt, it replaces the restored record without inheriting from it.
        start_live_session(
            &mut app,
            view_id,
            live_session(
                CLIAgent::Claude,
                true,
                Some(OTHER_SESSION_ID),
                Some("new task"),
            ),
        );
        let live = snapshot(&app, &terminal).expect("the live session is persisted");
        assert_eq!(live.session_id.as_deref(), Some(OTHER_SESSION_ID));
        assert_eq!(live.title.as_deref(), Some("new task"));
        assert_eq!(restorable(&app, &terminal), None);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Tracking);
    })
}

#[test]
fn unsupported_live_agent_keeps_restored_record() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());

        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Gemini, true, Some(OTHER_SESSION_ID), Some("hi")),
        );
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn resume_requested_before_bootstrap_keeps_record_for_another_crash() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());

        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        // The resume is pending, so the sidebar no longer offers it, but a crash before it runs
        // still persists the record.
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);
        assert_eq!(restorable(&app, &terminal), None);
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));

        assert_eq!(
            resume(&mut app, &terminal),
            CLIAgentResumeOutcome::NothingToResume
        );
    })
}

#[test]
fn resume_is_refused_while_the_pane_is_busy() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        simulate_running_command(&mut app, &terminal, "vim");

        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Busy);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn resume_never_overwrites_a_draft() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        type_draft(&mut app, &terminal);

        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::HasDraft);
        assert_eq!(input_text(&app, &terminal), "half-typed command");
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn resume_is_refused_when_the_session_is_open_in_another_pane() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        // The same session was already resumed (or is running) in another pane.
        let other_pane = EntityId::new();
        start_live_session(
            &mut app,
            other_pane,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );

        assert_eq!(
            resume(&mut app, &terminal),
            CLIAgentResumeOutcome::AlreadyOpen
        );
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));

        // Another agent's session with the same id doesn't count.
        start_live_session(
            &mut app,
            other_pane,
            live_session(CLIAgent::Codex, true, Some(SESSION_ID), Some("fix it")),
        );
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
    })
}

#[test]
fn queued_resume_is_offered_again_when_the_session_opens_elsewhere() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        // e.g. another pane resumed it while this one bootstrapped.
        start_live_session(
            &mut app,
            EntityId::new(),
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );

        wait_for(&app, |app| has_banner(app, &terminal)).await;

        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(in_flight_step(&app, &terminal), None);
        assert_eq!(input_text(&app, &terminal), "");
    })
}

#[test]
fn clearing_the_block_list_puts_the_banner_back() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        let banner = banner_in_block_list(&app, &terminal).expect("the banner is shown");

        // Cmd-K drops all rich content, the banner included.
        terminal.update(&mut app, |view, ctx| view.clear_buffer_for_testing(ctx));

        let new_banner = banner_in_block_list(&app, &terminal).expect("the banner is shown again");
        assert_ne!(new_banner, banner);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);

        // Once the session is no longer offered, clearing doesn't bring it back.
        terminal.update(&mut app, |view, ctx| {
            view.dismiss_restored_cli_agent_session(ctx);
        });
        terminal.update(&mut app, |view, ctx| view.clear_buffer_for_testing(ctx));
        assert!(!has_banner(&app, &terminal));
    })
}

#[test]
fn cleared_banner_keeps_what_it_said_about_the_last_resume() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);
        time_out(&mut app, &terminal);
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Failed
        );

        terminal.update(&mut app, |view, ctx| view.clear_buffer_for_testing(ctx));
        assert!(banner_in_block_list(&app, &terminal).is_some());
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Failed
        );
    })
}

#[test]
fn typing_between_resume_steps_stops_the_resume() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        let agent_command = agent_command();
        mark_resume_step_running(
            &mut app,
            &terminal,
            ResumeStep::ChangeDirectory,
            &[(ResumeStep::Agent, &agent_command)],
        );

        // The user starts typing while `cd` runs; the agent command must not be merged in.
        type_draft(&mut app, &terminal);
        complete_agent_block(&mut app, &terminal, true);
        terminal.update(&mut app, |view, _| {
            // The view resets this right after the hook.
            view.awaiting_pending_command_completion = false;
        });
        process_terminal_events().await;

        assert_eq!(input_text(&app, &terminal), "half-typed command");
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(has_banner(&app, &terminal));
        terminal.read(&app, |view, _| {
            assert!(
                view.cli_agent_session_restore
                    .pending_resume_steps
                    .is_empty()
            );
        });
    })
}

#[test]
fn agent_step_follows_a_successful_cd_into_an_empty_input() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        let agent_command = agent_command();
        mark_resume_step_running(
            &mut app,
            &terminal,
            ResumeStep::ChangeDirectory,
            &[(ResumeStep::Agent, &agent_command)],
        );

        complete_agent_block(&mut app, &terminal, true);
        terminal.update(&mut app, |view, _| {
            view.awaiting_pending_command_completion = false;
        });
        process_terminal_events().await;

        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        assert_eq!(in_flight_step(&app, &terminal), Some(ResumeStep::Agent));
        assert!(command_was_sent(&app, &terminal, &agent_command));
    })
}

#[test]
fn failed_cd_offers_the_session_again() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        let agent_command = agent_command();
        mark_resume_step_running(
            &mut app,
            &terminal,
            ResumeStep::ChangeDirectory,
            &[(ResumeStep::Agent, &agent_command)],
        );

        // e.g. the launch directory was removed since it was checked.
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(has_banner(&app, &terminal));
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Failed
        );
    })
}

#[test]
fn remote_sessions_are_informational_only() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let mut record = restored_record();
        record.remote_host = Some("me@devbox".to_owned());
        restore(&mut app, &terminal, record.clone());

        assert!(!can_resume_here(&app, &terminal));
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::CopyOnly);
        assert_eq!(restorable(&app, &terminal), Some(record));
    })
}

#[test]
fn resumed_session_is_acknowledged_by_its_session_id() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);

        // Command detection picks up `claude --resume` before the plugin reports anything.
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, false, None, None),
        );
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));

        // The plugin reports the same session: the resume is acknowledged and the record
        // follows the live session, keeping the restored title until a new prompt.
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), None),
        );
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Tracking);
        let live = snapshot(&app, &terminal).expect("the resumed session is persisted");
        assert_eq!(live.session_id.as_deref(), Some(SESSION_ID));
        assert_eq!(live.launch_cwd, Some(launch_dir()));
        assert_eq!(live.title.as_deref(), Some("fix the login bug"));
        assert!(live.can_resume_here());
    })
}

#[test]
fn failed_resume_offers_the_session_again() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);

        // The agent starts but can't find the session and exits with an error.
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, false, None, None),
        );
        end_live_session(&mut app, view_id);
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
        complete_agent_block(&mut app, &terminal, false);

        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(has_banner(&app, &terminal));
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Failed
        );
    })
}

#[test]
fn resume_command_that_never_ran_times_out_back_to_interrupted() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);

        time_out(&mut app, &terminal);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn timeout_waits_for_a_resume_command_that_is_still_running() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        // `claude --resume` is running but hasn't been detected or reported back yet.
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);

        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        assert!(has_resume_timer(&app, &terminal), "the timeout is re-armed");

        // The agent exits normally without ever reporting the session (e.g. it didn't find it,
        // or it has no plugin), so the resume isn't confirmed and the session stays on offer.
        complete_agent_block(&mut app, &terminal, true);
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert!(has_banner(&app, &terminal));
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Unconfirmed
        );
    })
}

#[test]
fn unacknowledged_exit_of_the_resume_command_keeps_an_interrupted_record() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);
        // The pane offered the session again while the resume command still ran.
        terminal.update(&mut app, |view, ctx| {
            view.set_cli_agent_recovery_phase(RecoveryPhase::Interrupted, ctx);
            view.show_cli_agent_session_restore_banner(PreviousResumeAttempt::Failed, ctx);
        });
        assert!(has_banner(&app, &terminal));

        // The agent exited normally, but never reported the restored session.
        complete_agent_block(&mut app, &terminal, true);
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert!(has_banner(&app, &terminal));
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::Unconfirmed
        );
    })
}

#[test]
fn unsent_resume_command_is_withdrawn_on_timeout() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);
        // The command was typed into the input, but the shell never accepted it.
        let agent_command = agent_command();
        terminal.update(&mut app, |view, ctx| {
            view.awaiting_pending_command_completion = false;
            view.input.update(ctx, |input, ctx| {
                input.set_pending_command(&agent_command, ctx);
            });
        });

        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(in_flight_step(&app, &terminal), None);
        assert!(
            !input_has_pending_command(&app, &terminal),
            "a later prompt must not run it"
        );
        assert_eq!(input_text(&app, &terminal), "");
        assert!(has_banner(&app, &terminal));
        // The session can be resumed again right away.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
    })
}

#[test]
fn withdrawn_resume_command_keeps_what_the_user_typed() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);
        let agent_command = agent_command();
        terminal.update(&mut app, |view, ctx| {
            view.awaiting_pending_command_completion = false;
            view.input.update(ctx, |input, ctx| {
                input.set_pending_command(&agent_command, ctx);
                input.system_insert(" --model opus", ctx);
            });
        });

        time_out(&mut app, &terminal);
        assert!(!input_has_pending_command(&app, &terminal));
        assert_eq!(
            input_text(&app, &terminal),
            format!("{agent_command} --model opus")
        );
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
    })
}

#[test]
fn successful_exit_after_an_acknowledged_resume_clears_the_record() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);

        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), None),
        );
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Tracking);
        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, true);
        assert_eq!(snapshot(&app, &terminal), None);
        assert_eq!(restorable(&app, &terminal), None);
    })
}

#[test]
fn failed_exit_after_an_acknowledged_resume_offers_the_session_again() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), None),
        );

        // The resumed agent crashed later on.
        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(
            restorable(&app, &terminal).and_then(|record| record.session_id),
            Some(SESSION_ID.to_owned())
        );
        assert_eq!(
            previous_resume(&app, &terminal),
            PreviousResumeAttempt::NotAttempted
        );
    })
}

/// Another terminal in the same app, in its own window.
fn another_terminal(app: &mut App) -> ViewHandle<TerminalView> {
    add_window_with_terminal(app, None)
}

fn enable_auto_resume(app: &mut App) {
    use ::settings::Setting as _;

    GeneralSettings::handle(app).update(app, |settings, ctx| {
        settings
            .auto_resume_cli_agent_sessions
            .set_value(true, ctx)
            .expect("the setting can be changed");
    });
}

#[test]
fn two_panes_never_resume_the_same_session() {
    App::test((), |mut app| async move {
        let first = terminal(&mut app);
        let second = another_terminal(&mut app);
        // e.g. the same session was restored into two panes.
        restore(&mut app, &first, restored_record());
        restore(&mut app, &second, restored_record());

        assert_eq!(resume(&mut app, &first), CLIAgentResumeOutcome::Resumed);
        assert_eq!(
            resume(&mut app, &second),
            CLIAgentResumeOutcome::AlreadyOpen
        );
        assert_eq!(restorable(&app, &second), Some(restored_record()));

        // Once the first pane stops resuming it, the second may.
        stop_resuming(&mut app, &first);
        assert_eq!(resume(&mut app, &second), CLIAgentResumeOutcome::Resumed);
        assert_eq!(resume(&mut app, &first), CLIAgentResumeOutcome::AlreadyOpen);
    })
}

/// Ends the pane's resume the way a failed one ends, offering the session again.
fn stop_resuming(app: &mut App, terminal: &ViewHandle<TerminalView>) {
    terminal.update(app, |view, ctx| {
        view.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx);
    });
}

#[test]
fn auto_resume_skips_a_session_another_pane_resumes() {
    App::test((), |mut app| async move {
        let first = terminal(&mut app);
        let second = another_terminal(&mut app);
        enable_auto_resume(&mut app);

        restore(&mut app, &first, restored_record());
        restore(&mut app, &second, restored_record());

        assert_eq!(phase(&app, &first), RecoveryPhase::ResumeQueued);
        assert_eq!(phase(&app, &second), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &second), Some(restored_record()));
    })
}

#[test]
fn only_evidently_interactive_sessions_are_resumed_automatically() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        enable_auto_resume(&mut app);

        // e.g. `claude -p` run by a script when Warp quit.
        let mut headless = restored_record();
        headless.evidently_interactive = false;
        restore(&mut app, &terminal, headless.clone());
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        // It can still be resumed by hand.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);

        restore(&mut app, &terminal, restored_record());
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumeQueued);
    })
}

#[test]
fn pane_tracking_the_session_takes_over_another_panes_offer() {
    App::test((), |mut app| async move {
        let offering = bootstrapped_terminal(&mut app).await;
        let running = another_terminal(&mut app);
        wait_for_bootstrap(&app, &running).await;
        let saves = count_saves(&mut app);
        restore(&mut app, &offering, restored_record());
        assert!(has_banner(&app, &offering));

        // The user resumes the session by hand in the other pane; until it does something, that
        // pane doesn't persist it, so the offer stays.
        start_live_session(
            &mut app,
            running.id(),
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), None),
        );
        process_terminal_events().await;
        assert_eq!(restorable(&app, &offering), Some(restored_record()));

        start_live_session(
            &mut app,
            running.id(),
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("keep going")),
        );
        assert_eq!(
            snapshot(&app, &running).and_then(|record| record.session_id),
            Some(SESSION_ID.to_owned())
        );
        wait_for(&app, |app| snapshot(app, &offering).is_none()).await;

        assert_eq!(snapshot(&app, &offering), None);
        assert_eq!(restorable(&app, &offering), None);
        assert!(!has_banner(&app, &offering));
        assert_eq!(saves.get(), 2, "both panes' records changed");
    })
}

#[test]
fn another_session_in_another_pane_keeps_the_offer() {
    App::test((), |mut app| async move {
        let offering = terminal(&mut app);
        let running = another_terminal(&mut app);
        restore(&mut app, &offering, restored_record());

        start_live_session(
            &mut app,
            running.id(),
            live_session(CLIAgent::Claude, true, Some(OTHER_SESSION_ID), Some("hi")),
        );
        process_terminal_events().await;
        assert_eq!(restorable(&app, &offering), Some(restored_record()));
    })
}

#[test]
fn headless_run_in_the_pane_keeps_the_offered_session() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());

        // `npm test` runs `claude -p`, whose plugin reports through this pane.
        simulate_running_command(&mut app, &terminal, "npm test");
        start_live_session(
            &mut app,
            view_id,
            live_session(
                CLIAgent::Claude,
                true,
                Some(OTHER_SESSION_ID),
                Some("summarize the failures"),
            ),
        );
        assert_eq!(snapshot(&app, &terminal), Some(restored_record()));
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert!(has_banner(&app, &terminal));

        // The script fails; the offered session is untouched.
        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn interactive_prompt_lets_a_session_replace_the_offered_one() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());

        // An agent started through something Warp doesn't recognize as the agent, e.g. a shell
        // function, that asks for a permission: only interactive sessions do.
        simulate_running_command(&mut app, &terminal, "my_claude_function");
        let mut session = live_session(
            CLIAgent::Claude,
            true,
            Some(OTHER_SESSION_ID),
            Some("new task"),
        );
        session.session_context.showed_interactive_prompt = true;
        start_live_session(&mut app, view_id, session);

        let live = snapshot(&app, &terminal).expect("the live session is persisted");
        assert_eq!(live.session_id.as_deref(), Some(OTHER_SESSION_ID));
        assert!(live.evidently_interactive);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Tracking);
    })
}

#[test]
fn failed_headless_run_is_not_offered() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        let saves = count_saves(&mut app);
        simulate_running_command(&mut app, &terminal, "./batch.sh");
        start_live_session(
            &mut app,
            view_id,
            live_session(
                CLIAgent::Claude,
                true,
                Some(SESSION_ID),
                Some("label the data"),
            ),
        );
        let live = snapshot(&app, &terminal).expect("headless runs are persisted while they run");
        assert!(!live.evidently_interactive);

        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(snapshot(&app, &terminal), None);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Tracking);
        assert!(!has_banner(&app, &terminal));
        assert_eq!(saves.get(), 2);
    })
}

#[test]
fn queued_resume_gives_up_when_the_shell_never_bootstraps() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert!(has_resume_timer(&app, &terminal));

        // The fallback's deadline passes without the shell bootstrapping.
        terminal.update(&mut app, |view, _| {
            view.cli_agent_session_restore.bootstrap_fallback_deadline = Some(Instant::now());
        });
        time_out(&mut app, &terminal);

        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        // The sidebar offers it again; the banner waits for the shell.
        terminal.read(&app, |view, _| {
            assert!(view.cli_agent_session_restore.show_banner_after_bootstrap);
        });
        // The resume can be requested again.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
    })
}

#[test]
fn escape_write_during_a_turn_is_observed() {
    App::test((), |mut app| async move {
        let terminal = bootstrapped_terminal(&mut app).await;
        let view_id = terminal.id();
        simulate_running_command(&mut app, &terminal, "claude");
        let mut session = live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it"));
        session.session_context.resume_turn_in_progress = true;
        start_live_session(&mut app, view_id, session);

        terminal.update(&mut app, |view, ctx| {
            view.write_user_bytes_to_pty(vec![0x1b], ctx);
        });
        let turn_in_progress = |app: &App| {
            CLIAgentSessionsModel::handle(app).read(app, |sessions, _| {
                sessions
                    .session(view_id)
                    .is_some_and(|session| session.session_context.resume_turn_in_progress)
            })
        };
        assert!(turn_in_progress(&app));
        wait_for(&app, |app| !turn_in_progress(app)).await;

        CLIAgentSessionsModel::handle(&app).read(&app, |sessions, _| {
            let session = sessions
                .session(view_id)
                .expect("the session is registered");
            assert!(
                !session.session_context.resume_turn_in_progress,
                "Escape went unanswered, so the turn is over"
            );
            assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
        });
    })
}
