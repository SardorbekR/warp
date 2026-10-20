use std::cell::Cell;
use std::rc::Rc;

use warpui::{App, EntityId, ViewHandle};

use super::*;
use crate::terminal::cli_agent_sessions::event::{
    CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType,
};
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
};
use crate::test_util::terminal::{add_window_with_terminal, initialize_app_for_terminal_view};

const SESSION_ID: &str = "3f2a9c1e-8d4b-4f6a-9e2d-1b7c5a0f8e3d";
const OTHER_SESSION_ID: &str = "9b1d2c3e-4f5a-4b6c-8d7e-0f1a2b3c4d5e";

fn terminal(app: &mut App) -> ViewHandle<TerminalView> {
    initialize_app_for_terminal_view(app);
    add_window_with_terminal(app, None)
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

fn restored_record() -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        agent: CLIAgent::Claude,
        session_id: Some(SESSION_ID.to_owned()),
        launch_cwd: Some("/Users/me/my repo".to_owned()),
        title: Some("fix the login bug".to_owned()),
        transcript_path: None,
        status: None,
        remote_host: None,
        launched_locally: true,
        updated_at_unix_ms: 1_000,
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

fn phase(app: &App, terminal: &ViewHandle<TerminalView>) -> RecoveryPhase {
    terminal.read(app, |view, _| view.cli_agent_session_restore.phase)
}

fn has_banner(app: &App, terminal: &ViewHandle<TerminalView>) -> bool {
    terminal.read(app, |view, _| {
        view.cli_agent_session_restore.banner.is_some()
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
            ResumeStep::ChangeDirectory => "cd '/Users/me/my repo'".to_owned(),
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

#[test]
fn restored_session_shows_banner_after_real_bootstrap() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        // `restore_pane_leaf` hands the record over before the shell has bootstrapped.
        terminal.read(&app, |view, _| assert!(!view.is_login_shell_bootstrapped));
        restore(&mut app, &terminal, restored_record());
        assert!(!has_banner(&app, &terminal));

        process_terminal_events().await;

        terminal.read(&app, |view, _| assert!(view.is_login_shell_bootstrapped));
        assert!(has_banner(&app, &terminal));
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
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

        process_terminal_events().await;

        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        // The pane isn't in the launch directory, so it `cd`s there first.
        assert_eq!(
            in_flight_step(&app, &terminal),
            Some(ResumeStep::ChangeDirectory)
        );
        assert!(command_was_sent(&app, &terminal, "cd '/Users/me/my repo'"));
        terminal.read(&app, |view, _| {
            let remaining: Vec<_> = view
                .cli_agent_session_restore
                .pending_resume_steps
                .iter()
                .cloned()
                .collect();
            assert_eq!(remaining, vec![(ResumeStep::Agent, agent_command())]);
        });
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

        process_terminal_events().await;

        assert_eq!(input_text(&app, &terminal), "half-typed command");
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert!(has_banner(&app, &terminal));
    })
}

#[test]
fn queued_resume_that_never_runs_times_out_to_the_banner() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert_eq!(restorable(&app, &terminal), None);

        // The shell never got ready within the queue timeout.
        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(has_banner(&app, &terminal));
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
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        let changes = count_state_changes(&mut app, &terminal);

        restore(&mut app, &terminal, restored_record());
        assert_eq!(changes.get(), 1, "entered Interrupted");

        // Resuming leaves Interrupted; the timeout brings it back.
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Resumed);
        assert_eq!(changes.get(), 2);
        time_out(&mut app, &terminal);
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

#[test]
fn status_flips_do_not_save() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        let saves = count_saves(&mut app);
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );
        assert_eq!(saves.get(), 1);

        for _ in 0..5 {
            flip_status(&mut app, view_id);
        }
        assert_eq!(saves.get(), 1);
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
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
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
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        process_terminal_events().await;

        terminal.update(&mut app, |view, _| {
            view.model
                .lock()
                .simulate_long_running_block("claude --model opus", "");
        });
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );
        let live = snapshot(&app, &terminal).expect("the live session is persisted");
        assert!(live.launched_locally);
        assert!(live.can_resume_here());
    })
}

#[test]
fn agent_reported_from_another_command_is_copy_only() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        process_terminal_events().await;

        // e.g. the agent runs on the other side of an SSH session Warp didn't take over, and
        // its plugin reports through this pane.
        terminal.update(&mut app, |view, _| {
            view.model
                .lock()
                .simulate_long_running_block("ssh devbox", "");
        });
        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), Some("fix it")),
        );
        let live = snapshot(&app, &terminal).expect("the live session is persisted");
        assert!(!live.launched_locally);

        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(restorable(&app, &terminal), Some(live));
        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::CopyOnly);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
    })
}

#[test]
fn new_agent_session_with_activity_supersedes_restored_record() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());

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
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        terminal.update(&mut app, |view, _| {
            view.model.lock().simulate_long_running_block("vim", "");
        });

        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::Busy);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn resume_never_overwrites_a_draft() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        type_draft(&mut app, &terminal);

        assert_eq!(resume(&mut app, &terminal), CLIAgentResumeOutcome::HasDraft);
        assert_eq!(input_text(&app, &terminal), "half-typed command");
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn typing_between_resume_steps_stops_the_resume() {
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
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        let agent_command = agent_command();
        mark_resume_step_running(
            &mut app,
            &terminal,
            ResumeStep::ChangeDirectory,
            &[(ResumeStep::Agent, &agent_command)],
        );

        // e.g. the launch directory no longer exists.
        complete_agent_block(&mut app, &terminal, false);
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
        assert!(has_banner(&app, &terminal));
    })
}

#[test]
fn remote_sessions_are_informational_only() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let mut record = restored_record();
        record.remote_host = Some("me@devbox".to_owned());
        restore(&mut app, &terminal, record.clone());

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
        assert_eq!(live.launch_cwd.as_deref(), Some("/Users/me/my repo"));
        assert_eq!(live.title.as_deref(), Some("fix the login bug"));
        assert!(live.can_resume_here());
    })
}

#[test]
fn failed_resume_offers_the_session_again() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
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
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        // `claude --resume` is running but hasn't been detected or reported back yet.
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);

        time_out(&mut app, &terminal);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::ResumePending);
        terminal.read(&app, |view, _| {
            assert!(
                view.cli_agent_session_restore.resume_timer.is_some(),
                "the timeout is re-armed"
            );
        });

        // The user quits the resumed agent normally.
        complete_agent_block(&mut app, &terminal, true);
        assert_eq!(snapshot(&app, &terminal), None);
    })
}

#[test]
fn successful_exit_of_the_resume_command_clears_an_interrupted_record() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        mark_resume_step_running(&mut app, &terminal, ResumeStep::Agent, &[]);
        // The pane offered the session again while the resume command still ran.
        terminal.update(&mut app, |view, ctx| {
            view.set_cli_agent_recovery_phase(RecoveryPhase::Interrupted, ctx);
            view.show_cli_agent_session_restore_banner(false, ctx);
        });
        assert!(has_banner(&app, &terminal));

        // The resumed agent was quit normally.
        complete_agent_block(&mut app, &terminal, true);
        assert_eq!(snapshot(&app, &terminal), None);
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Tracking);
        assert!(!has_banner(&app, &terminal));
    })
}

#[test]
fn unsent_resume_command_is_withdrawn_on_timeout() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
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
fn successful_exit_after_resume_clears_the_record() {
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
