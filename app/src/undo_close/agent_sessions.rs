//! Workspace actions for resuming CLI agent sessions (Claude Code, Codex) that were restored into
//! panes after Warp quit or crashed.

use ::settings::ToggleableSetting as _;
use warp_errors::report_if_error;
use warpui::keymap::EditableBinding;
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext};

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
/// skipping panes where that could disturb something (a running command or typed input) and
/// sessions that are open, or being resumed, in another pane: each session is resumed once.
/// Returns the terminal views that resume their session.
pub fn resume_all_cli_agent_sessions(
    workspace: &Workspace,
    ctx: &mut ViewContext<Workspace>,
) -> Vec<EntityId> {
    let terminal_views: Vec<_> = workspace
        .tab_views()
        .flat_map(|pane_group| pane_group.as_ref(ctx).terminal_views(ctx))
        .collect();

    // Each pane's resume checks the sessions other panes run, resume or track (see
    // `CLIAgentSessionsModel::resume_claim_elsewhere`), so each session is resumed once.
    let mut resumed_views = Vec::new();
    let mut skipped = 0usize;
    for terminal_view in terminal_views {
        let outcome =
            terminal_view.update(ctx, |view, ctx| view.resume_restored_cli_agent_session(ctx));
        match outcome {
            CLIAgentResumeOutcome::Resumed => resumed_views.push(terminal_view.id()),
            CLIAgentResumeOutcome::Busy
            | CLIAgentResumeOutcome::HasDraft
            | CLIAgentResumeOutcome::AlreadyOpen
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
    const SKIP_REASONS: &str =
        "busy, unsent input, already open in another pane, or not resumable from this pane";
    let message = match (resumed_views.len(), skipped) {
        (0, 0) => "No agent sessions to resume".to_owned(),
        (resumed, 0) => format!("Resuming {}", sessions(resumed)),
        (0, skipped) => format!("Couldn't resume {}: {SKIP_REASONS}", sessions(skipped)),
        (resumed, skipped) => format!(
            "Resuming {}; skipped {} ({SKIP_REASONS})",
            sessions(resumed),
            sessions(skipped)
        ),
    };
    let window_id = ctx.window_id();
    ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        toast_stack.add_ephemeral_toast(DismissibleToast::default(message), window_id, ctx);
    });
    resumed_views
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
