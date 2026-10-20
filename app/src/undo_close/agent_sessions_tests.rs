use std::collections::HashMap;
use std::sync::Arc;

use warpui::platform::WindowStyle;
use warpui::{App, ViewHandle};

use super::*;
use crate::GlobalResourceHandles;
use crate::root_view::NewWorkspaceSource;
use crate::terminal::cli_agent_sessions::restore::PersistedCLIAgentSession;
use crate::terminal::{CLIAgent, TerminalView};
use crate::workspace::view::tests::{initialize_app, mock_workspace};

fn record(launched_locally: bool) -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        agent: CLIAgent::Claude,
        session_id: Some("3f2a9c1e-8d4b-4f6a-9e2d-1b7c5a0f8e3d".to_owned()),
        launch_cwd: Some("/Users/me/repo".to_owned()),
        title: Some("fix the login bug".to_owned()),
        transcript_path: None,
        status: None,
        remote_host: None,
        launched_locally,
        updated_at_unix_ms: 1_000,
    }
}

fn active_terminal(app: &App, workspace: &ViewHandle<Workspace>) -> ViewHandle<TerminalView> {
    workspace.read(app, |workspace, ctx| {
        workspace
            .active_tab_pane_group()
            .as_ref(ctx)
            .active_session_view(ctx)
            .expect("the tab has a terminal")
    })
}

fn restorable(app: &App, terminal: &ViewHandle<TerminalView>) -> Option<PersistedCLIAgentSession> {
    terminal.read(app, |view, _| view.restorable_cli_agent_session().cloned())
}

fn restored_workspace(
    app: &mut App,
    window_snapshot: crate::app_state::WindowSnapshot,
) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Restored {
                window_snapshot,
                block_lists: Arc::new(HashMap::new()),
            },
            ctx,
        )
    });
    workspace
}

#[test]
fn snapshot_round_trip_restores_the_session_into_its_pane() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let terminal = active_terminal(&app, &workspace);
        terminal.update(&mut app, |view, ctx| {
            view.restore_cli_agent_session(record(true), ctx);
        });

        let window_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.snapshot(ctx.window_id(), false, ctx)
        });
        let restored = restored_workspace(&mut app, window_snapshot);

        let restored_terminal = active_terminal(&app, &restored);
        assert_ne!(restored_terminal.id(), terminal.id());
        assert_eq!(restorable(&app, &restored_terminal), Some(record(true)));
    })
}

#[test]
fn resume_all_resumes_local_sessions_and_skips_copy_only_ones() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let terminal = active_terminal(&app, &workspace);

        // A session that didn't start from the pane's shell can't be resumed by it.
        terminal.update(&mut app, |view, ctx| {
            view.restore_cli_agent_session(record(false), ctx);
        });
        workspace.update(&mut app, |workspace, ctx| {
            resume_all_cli_agent_sessions(workspace, ctx)
        });
        assert_eq!(restorable(&app, &terminal), Some(record(false)));

        terminal.update(&mut app, |view, ctx| {
            view.restore_cli_agent_session(record(true), ctx);
        });
        workspace.update(&mut app, |workspace, ctx| {
            resume_all_cli_agent_sessions(workspace, ctx)
        });
        // The resume is queued or running, so the pane no longer offers it, but it still
        // persists the record in case of another crash.
        assert_eq!(restorable(&app, &terminal), None);
        assert_eq!(
            terminal.read(&app, |view, _| view.cli_agent_session_for_snapshot()),
            Some(record(true))
        );
    })
}
