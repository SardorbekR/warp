use std::cell::Cell;
use std::rc::Rc;

use warpui::{App, EntityId, ViewHandle};

use super::*;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::restore::PersistedCLIAgentStatus;
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

fn restored_record() -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        agent: CLIAgent::Claude,
        session_id: Some(SESSION_ID.to_owned()),
        launch_cwd: Some("/Users/me/my repo".to_owned()),
        title: Some("fix the login bug".to_owned()),
        transcript_path: None,
        status: Some(PersistedCLIAgentStatus::Blocked),
        remote_host: None,
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

fn snapshot(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<PersistedCLIAgentSession> {
    terminal.read(app, |view, _| view.cli_agent_session_for_snapshot())
}

fn restorable(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<PersistedCLIAgentSession> {
    terminal.read(app, |view, _| view.restorable_cli_agent_session().cloned())
}

fn phase(app: &App, terminal: &ViewHandle<TerminalView>) -> RecoveryPhase {
    terminal.read(app, |view, _| view.cli_agent_session_restore.phase)
}

/// Puts the pane in the state right after the resume commands were sent to the shell.
fn mark_resume_sent(app: &mut App, terminal: &ViewHandle<TerminalView>) {
    terminal.update(app, |view, _| {
        view.cli_agent_session_restore.phase = RecoveryPhase::ResumePending;
    });
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
        let restorable = restorable(&app, &terminal).expect("the session can be resumed");
        assert_eq!(restorable.session_id.as_deref(), Some(SESSION_ID));
        terminal.read(&app, |view, _| {
            assert!(view.cli_agent_session_restore.banner.is_some());
        });
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
        terminal.update(&mut app, |view, _| view.is_login_shell_bootstrapped = false);
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
fn queued_resume_falls_back_to_the_banner_when_input_has_a_draft() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        terminal.update(&mut app, |view, _| {
            view.cli_agent_session_restore.phase = RecoveryPhase::ResumeQueued;
        });
        type_draft(&mut app, &terminal);

        terminal.update(&mut app, |view, ctx| {
            view.on_bootstrap_precmd_done_for_cli_agent_restore(ctx);
        });
        assert_eq!(input_text(&app, &terminal), "half-typed command");
        assert_eq!(phase(&app, &terminal), RecoveryPhase::Interrupted);
        terminal.read(&app, |view, _| {
            assert!(view.cli_agent_session_restore.banner.is_some());
        });
    })
}

#[test]
fn remote_sessions_are_informational_only() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let mut record = restored_record();
        record.remote_host = Some("me@devbox".to_owned());
        restore(&mut app, &terminal, record.clone());

        assert_eq!(
            resume(&mut app, &terminal),
            CLIAgentResumeOutcome::RemoteOnly
        );
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
    })
}

#[test]
fn failed_resume_offers_the_session_again() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);

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
        terminal.read(&app, |view, _| {
            assert!(view.cli_agent_session_restore.banner.is_some());
        });
    })
}

#[test]
fn resume_commands_finishing_without_an_agent_offer_the_session_again() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);

        // e.g. `cd` failed, or `claude` wasn't found.
        terminal.update(&mut app, |view, ctx| {
            view.on_pending_command_completed_for_cli_agent_restore(ctx);
        });
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn unacknowledged_resume_times_out_back_to_interrupted() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);

        terminal.update(&mut app, |view, ctx| {
            view.on_cli_agent_resume_acknowledgement_timeout(ctx);
        });
        assert_eq!(restorable(&app, &terminal), Some(restored_record()));
    })
}

#[test]
fn successful_exit_after_resume_clears_the_record() {
    App::test((), |mut app| async move {
        let terminal = terminal(&mut app);
        let view_id = terminal.id();
        restore(&mut app, &terminal, restored_record());
        mark_resume_sent(&mut app, &terminal);

        start_live_session(
            &mut app,
            view_id,
            live_session(CLIAgent::Claude, true, Some(SESSION_ID), None),
        );
        end_live_session(&mut app, view_id);
        complete_agent_block(&mut app, &terminal, true);
        terminal.update(&mut app, |view, ctx| {
            view.on_pending_command_completed_for_cli_agent_restore(ctx);
        });
        assert_eq!(snapshot(&app, &terminal), None);
        assert_eq!(restorable(&app, &terminal), None);
    })
}
