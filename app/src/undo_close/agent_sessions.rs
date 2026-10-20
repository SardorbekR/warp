//! Workspace actions for resuming CLI agent sessions (Claude Code, Codex) that were restored into
//! panes after Warp quit or crashed.

use ::settings::ToggleableSetting as _;
use warp_errors::report_if_error;
use warpui::keymap::EditableBinding;
use warpui::{AppContext, SingletonEntity, ViewContext};

use crate::settings_view::ToggleSettingActionPair;
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::view::cli_agent_session_restore::CLIAgentResumeOutcome;
use crate::view_components::DismissibleToast;
use crate::workspace::{ToastStack, Workspace, WorkspaceAction};

/// Present in the workspace keymap context while `general.auto_resume_cli_agent_sessions` is on.
pub const AUTO_RESUME_CLI_AGENT_SESSIONS_CONTEXT_FLAG: &str = "Auto_Resume_CLI_Agent_Sessions";

pub(super) fn init(ctx: &mut AppContext) {
    use warpui::keymap::macros::*;

    crate::terminal::cli_agent_sessions::restore_save::init(ctx);

    ctx.register_editable_bindings([EditableBinding::new(
        "workspace:resume_all_cli_agent_sessions",
        "Resume all agent sessions",
        WorkspaceAction::ResumeAllCLIAgentSessions,
    )
    .with_context_predicate(id!("Workspace"))]);

    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "auto-resume of agent sessions on startup",
            WorkspaceAction::ToggleAutoResumeCLIAgentSessions,
            &id!("Workspace"),
            AUTO_RESUME_CLI_AGENT_SESSIONS_CONTEXT_FLAG,
        )],
        ctx,
    );
}

/// Resumes the interrupted CLI agent session in every terminal pane of the workspace's window,
/// skipping panes where that could disturb something (a running command or typed input).
pub fn resume_all_cli_agent_sessions(workspace: &Workspace, ctx: &mut ViewContext<Workspace>) {
    let terminal_views: Vec<_> = workspace
        .tab_views()
        .flat_map(|pane_group| pane_group.as_ref(ctx).terminal_views(ctx))
        .collect();

    let mut resumed = 0usize;
    let mut skipped = 0usize;
    for terminal_view in terminal_views {
        let outcome =
            terminal_view.update(ctx, |view, ctx| view.resume_restored_cli_agent_session(ctx));
        match outcome {
            CLIAgentResumeOutcome::Resumed => resumed = resumed.saturating_add(1),
            CLIAgentResumeOutcome::Busy
            | CLIAgentResumeOutcome::HasDraft
            | CLIAgentResumeOutcome::CopyOnly => skipped = skipped.saturating_add(1),
            CLIAgentResumeOutcome::NothingToResume => {}
        }
    }

    let sessions = |count: usize| {
        if count == 1 {
            "1 agent session".to_owned()
        } else {
            format!("{count} agent sessions")
        }
    };
    let message = match (resumed, skipped) {
        (0, 0) => "No agent sessions to resume".to_owned(),
        (resumed, 0) => format!("Resuming {}", sessions(resumed)),
        (0, skipped) => format!(
            "Couldn't resume {}: busy, unsent input, or not started from the pane's shell",
            sessions(skipped)
        ),
        (resumed, skipped) => format!(
            "Resuming {}; skipped {} (busy, unsent input, or not started from the pane's shell)",
            sessions(resumed),
            sessions(skipped)
        ),
    };
    let window_id = ctx.window_id();
    ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        toast_stack.add_ephemeral_toast(DismissibleToast::default(message), window_id, ctx);
    });
}

pub fn toggle_auto_resume_cli_agent_sessions(ctx: &mut ViewContext<Workspace>) {
    GeneralSettings::handle(ctx).update(ctx, |general_settings, ctx| {
        report_if_error!(
            general_settings
                .auto_resume_cli_agent_sessions
                .toggle_and_save_value(ctx)
        );
    });
}

#[cfg(test)]
#[path = "agent_sessions_tests.rs"]
mod tests;
