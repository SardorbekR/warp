//! Crash recovery for CLI agent sessions (Claude Code, Codex) in a terminal pane.
//!
//! The pane keeps a [`PersistedCLIAgentSession`] record that is saved with the app state. Its
//! lifecycle:
//!
//! - **Tracking**: the record follows the agent session running in the pane. It's kept when the
//!   session ends without its command block finishing (e.g. the pane was closed with undo
//!   available, or the session was replaced) and is cleared only when the agent's block exits
//!   successfully, i.e. the user quit the agent deliberately. A block that fails leaves the
//!   record **Interrupted**.
//! - **Interrupted**: restored from a previous run (Warp quit or crashed while the agent ran) or
//!   left by an agent that exited with an error. The pane offers to resume it with an inline
//!   banner, and the sidebar can show it. A new agent session with activity in the pane
//!   supersedes it; Dismiss clears it.
//! - **ResumeQueued** / **ResumePending**: a resume was requested and waits for the shell, or its
//!   commands were sent. The record is kept, so another crash before the resumed agent reports
//!   back still has it. The resume is acknowledged once the agent reports the same session id;
//!   if the commands fail, or nothing is running after [`RESUME_ACKNOWLEDGEMENT_TIMEOUT`], the
//!   record goes back to Interrupted.

use std::time::Duration;

use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::clipboard::ClipboardContent;
use warpui::{SingletonEntity, ViewContext, ViewHandle};

use crate::terminal::TerminalView;
use crate::terminal::cli_agent_sessions::restore::{
    PersistedCLIAgentSession, is_resumable_agent, reported_session_id,
};
use crate::terminal::cli_agent_sessions::restore_save::CLIAgentRestoreSaveScheduler;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionsModel, CLIAgentSessionsModelEvent};
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::shell::ShellType;
use crate::terminal::view::cli_agent_session_restore_banner::{
    CLIAgentSessionRestoreBanner, CLIAgentSessionRestoreBannerEvent,
};
use crate::terminal::view::{RichContentInsertionPosition, RichContentMetadata};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

/// How long a sent resume may go without the agent starting before it's offered again.
const RESUME_ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RecoveryPhase {
    /// The record (if any) follows the agent session running in this pane.
    #[default]
    Tracking,
    /// The record describes a session that was interrupted and can be resumed here.
    Interrupted,
    /// Resume was requested before the shell was ready; it runs once bootstrapping finishes.
    ResumeQueued,
    /// The resume commands were sent; waiting for the agent to report the same session.
    ResumePending,
}

#[derive(Default)]
pub(super) struct CLIAgentSessionRestoreState {
    record: Option<PersistedCLIAgentSession>,
    phase: RecoveryPhase,
    /// Whether `record` was built from the agent session instance currently registered for
    /// this pane, so later updates of that instance may inherit its fields.
    record_is_live_instance: bool,
    /// Whether an agent session was seen during the command block that's running now.
    agent_block_active: bool,
    show_banner_after_bootstrap: bool,
    banner: Option<ViewHandle<CLIAgentSessionRestoreBanner>>,
    resume_acknowledgement_timer: Option<SpawnedFutureHandle>,
}

impl CLIAgentSessionRestoreState {
    fn cancel_resume_acknowledgement_timer(&mut self) {
        if let Some(timer) = self.resume_acknowledgement_timer.take() {
            timer.abort();
        }
    }
}

/// Result of asking a pane to resume its restored CLI agent session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CLIAgentResumeOutcome {
    /// The resume commands were sent, or will be once the shell is ready.
    Resumed,
    /// There's no interrupted session waiting to be resumed.
    NothingToResume,
    /// Something is running or pending in the pane.
    Busy,
    /// The input editor has text the user typed, which resuming would overwrite.
    HasDraft,
    /// The session ran on a remote host; the pane can only show it and copy its command.
    RemoteOnly,
}

fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn same_record(
    before: Option<&PersistedCLIAgentSession>,
    after: Option<&PersistedCLIAgentSession>,
) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => before.has_same_content(after),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

impl TerminalView {
    /// The CLI agent session to persist for this pane, in any recovery phase.
    pub(crate) fn cli_agent_session_for_snapshot(&self) -> Option<PersistedCLIAgentSession> {
        self.cli_agent_session_restore.record.clone()
    }

    /// The interrupted CLI agent session (e.g. restored after Warp quit or crashed) that can be
    /// resumed in this pane, if any. Cheap; suitable for calling while rendering. A record with
    /// a `remote_host` ran over SSH and is informational only.
    pub(crate) fn restorable_cli_agent_session(&self) -> Option<&PersistedCLIAgentSession> {
        let state = &self.cli_agent_session_restore;
        state
            .record
            .as_ref()
            .filter(|_| state.phase == RecoveryPhase::Interrupted)
    }

    /// Hands this pane the CLI agent session that was running in it when the app state was last
    /// saved. Depending on `general.auto_resume_cli_agent_sessions`, the session is resumed or
    /// offered for resumption once the shell has bootstrapped.
    pub(crate) fn restore_cli_agent_session(
        &mut self,
        record: PersistedCLIAgentSession,
        ctx: &mut ViewContext<Self>,
    ) {
        if !is_resumable_agent(record.agent) {
            return;
        }
        let auto_resume =
            *GeneralSettings::as_ref(ctx).auto_resume_cli_agent_sessions && !record.is_remote();
        self.remove_cli_agent_session_restore_banner(ctx);
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume_acknowledgement_timer();
        state.record = Some(record);
        state.record_is_live_instance = false;
        if auto_resume {
            state.phase = RecoveryPhase::ResumeQueued;
            state.show_banner_after_bootstrap = false;
        } else {
            state.phase = RecoveryPhase::Interrupted;
            state.show_banner_after_bootstrap = true;
        }
    }

    /// Resumes the interrupted CLI agent session in this pane, unless that could disturb
    /// something: a running or pending command, or text the user typed into the input.
    pub(crate) fn resume_restored_cli_agent_session(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) -> CLIAgentResumeOutcome {
        let state = &self.cli_agent_session_restore;
        let Some(record) = state.record.as_ref() else {
            return CLIAgentResumeOutcome::NothingToResume;
        };
        if state.phase != RecoveryPhase::Interrupted {
            return CLIAgentResumeOutcome::NothingToResume;
        }
        if record.is_remote() {
            return CLIAgentResumeOutcome::RemoteOnly;
        }
        if let Some(blocker) = self.cli_agent_resume_blocker(ctx) {
            return blocker;
        }

        self.remove_cli_agent_session_restore_banner(ctx);
        self.cli_agent_session_restore.show_banner_after_bootstrap = false;
        if self.is_login_shell_bootstrapped && self.active_session_shell_type(ctx).is_some() {
            self.send_cli_agent_resume_commands(ctx);
        } else {
            self.cli_agent_session_restore.phase = RecoveryPhase::ResumeQueued;
        }
        ctx.notify();
        CLIAgentResumeOutcome::Resumed
    }

    /// Why the resume commands can't be typed into this pane right now, if they can't.
    fn cli_agent_resume_blocker(&self, ctx: &ViewContext<Self>) -> Option<CLIAgentResumeOutcome> {
        let busy = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .is_some()
            || self.has_pending_command_or_awaiting_completion(ctx)
            || self.is_long_running();
        if busy {
            return Some(CLIAgentResumeOutcome::Busy);
        }
        // The pending command is inserted into the editor, so it would mix with a draft.
        if !self.input.as_ref(ctx).buffer_text(ctx).is_empty() {
            return Some(CLIAgentResumeOutcome::HasDraft);
        }
        None
    }

    /// Queues the commands that resume the interrupted session and starts running them.
    fn send_cli_agent_resume_commands(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(record) = self.cli_agent_session_restore.record.as_ref() else {
            return;
        };
        let shell_type = self.active_session_shell_type(ctx).unwrap_or_else(|| {
            log::warn!("Shell type unknown when resuming a CLI agent session; assuming bash");
            ShellType::Bash
        });
        let pane_cwd = self.pwd();
        let Some(steps) = record.resume_command_steps(shell_type, pane_cwd.as_deref()) else {
            return;
        };
        let state = &mut self.cli_agent_session_restore;
        state.phase = RecoveryPhase::ResumePending;
        state.agent_block_active = false;
        state.cancel_resume_acknowledgement_timer();
        state.resume_acknowledgement_timer = Some(ctx.spawn(
            async {
                Timer::after(RESUME_ACKNOWLEDGEMENT_TIMEOUT).await;
            },
            |me, _, ctx| me.on_cli_agent_resume_acknowledgement_timeout(ctx),
        ));
        self.set_pending_command_queue(steps, ctx);
        self.execute_pending_command((), ctx);
    }

    fn on_cli_agent_resume_acknowledgement_timeout(&mut self, ctx: &mut ViewContext<Self>) {
        let state = &mut self.cli_agent_session_restore;
        state.resume_acknowledgement_timer = None;
        if state.phase != RecoveryPhase::ResumePending {
            return;
        }
        // An agent that's running but hasn't reported its session id is settled by its command
        // block finishing instead.
        if CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .is_some()
        {
            return;
        }
        self.mark_cli_agent_session_interrupted(true, ctx);
    }

    /// Updates the record from the agent session in this pane and saves the app state when the
    /// record changes.
    pub(super) fn track_cli_agent_session_for_restore(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            CLIAgentSessionsModelEvent::Started { .. }
            | CLIAgentSessionsModelEvent::StatusChanged { .. }
            | CLIAgentSessionsModelEvent::SessionUpdated { .. }
            | CLIAgentSessionsModelEvent::Ended { .. } => {}
            CLIAgentSessionsModelEvent::InputSessionChanged { .. } => return,
        }
        if event.terminal_view_id() != self.view_id {
            return;
        }

        let before = self.cli_agent_session_for_snapshot();
        let pane_cwd = self.pwd();
        let now = now_unix_ms();
        let state = &mut self.cli_agent_session_restore;
        if matches!(event, CLIAgentSessionsModelEvent::Ended { .. }) {
            // Whatever is registered now (if anything) is a different session instance. Ending
            // alone isn't a deliberate exit, so the record stays until the block finishes.
            state.record_is_live_instance = false;
        }

        // Read the model rather than the event: events are delivered after the fact, and the
        // model has the latest state (e.g. the replacement after a session was replaced).
        let mut superseded = false;
        if let Some(session) = CLIAgentSessionsModel::as_ref(ctx).session(self.view_id) {
            state.agent_block_active = true;
            match state.phase {
                RecoveryPhase::Tracking => {
                    let previous = state
                        .record
                        .as_ref()
                        .filter(|_| state.record_is_live_instance);
                    if let Some(live) = PersistedCLIAgentSession::from_live_session(
                        session,
                        pane_cwd.as_deref(),
                        previous,
                        now,
                    ) {
                        state.record = Some(live);
                        state.record_is_live_instance = true;
                    }
                }
                RecoveryPhase::Interrupted | RecoveryPhase::ResumeQueued => {
                    // A new agent session that has done something replaces the interrupted one.
                    if let Some(live) = PersistedCLIAgentSession::from_live_session(
                        session,
                        pane_cwd.as_deref(),
                        None,
                        now,
                    ) {
                        state.record = Some(live);
                        state.record_is_live_instance = true;
                        state.phase = RecoveryPhase::Tracking;
                        superseded = true;
                    }
                }
                RecoveryPhase::ResumePending => {
                    let restored = state.record.as_ref();
                    let acknowledged = restored.is_some_and(|restored| {
                        restored.agent == session.agent
                            && restored.session_id.as_deref() == reported_session_id(session)
                    });
                    let live = PersistedCLIAgentSession::from_live_session(
                        session,
                        pane_cwd.as_deref(),
                        restored,
                        now,
                    );
                    // The agent may also have moved on to another session with activity (e.g.
                    // the user picked a different one); that replaces the restored record too.
                    let moved_on = live.as_ref().is_some_and(|live| {
                        restored.is_none_or(|restored| live.session_id != restored.session_id)
                    });
                    if acknowledged || moved_on {
                        if let Some(live) = live {
                            state.record = Some(live);
                        }
                        state.record_is_live_instance = true;
                        state.phase = RecoveryPhase::Tracking;
                        state.cancel_resume_acknowledgement_timer();
                    }
                }
            }
        }

        if superseded {
            self.cli_agent_session_restore.show_banner_after_bootstrap = false;
            self.remove_cli_agent_session_restore_banner(ctx);
        }
        self.save_if_cli_agent_record_changed(before, ctx);
    }

    /// Settles the record when a command block that ran an agent session finishes: a successful
    /// exit is a deliberate quit, anything else leaves the session interrupted.
    pub(super) fn on_user_block_completed_for_cli_agent_restore(
        &mut self,
        succeeded: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        if !std::mem::take(&mut state.agent_block_active) {
            return;
        }
        state.record_is_live_instance = false;
        let was_resuming = match state.phase {
            RecoveryPhase::Tracking => false,
            RecoveryPhase::ResumePending => true,
            RecoveryPhase::Interrupted | RecoveryPhase::ResumeQueued => return,
        };
        if state.record.is_none() {
            return;
        }
        if succeeded {
            self.clear_cli_agent_session_record(ctx);
        } else {
            self.mark_cli_agent_session_interrupted(was_resuming, ctx);
        }
    }

    /// Runs a resume that was waiting for the shell, or shows the restore banner.
    pub(super) fn on_bootstrap_precmd_done_for_cli_agent_restore(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        let show_banner =
            std::mem::take(&mut self.cli_agent_session_restore.show_banner_after_bootstrap);
        if self.cli_agent_session_restore.record.is_none() {
            return;
        }
        match self.cli_agent_session_restore.phase {
            RecoveryPhase::ResumeQueued => {
                if self.cli_agent_resume_blocker(ctx).is_some() {
                    // Never type over a draft or into a busy pane; let the user decide.
                    self.cli_agent_session_restore.phase = RecoveryPhase::Interrupted;
                    self.show_cli_agent_session_restore_banner(false, ctx);
                } else {
                    self.send_cli_agent_resume_commands(ctx);
                }
            }
            RecoveryPhase::Interrupted => {
                if show_banner {
                    self.show_cli_agent_session_restore_banner(false, ctx);
                }
            }
            RecoveryPhase::Tracking | RecoveryPhase::ResumePending => {}
        }
    }

    /// Offers the session again if the resume commands finished without the agent ever
    /// starting (e.g. the launch directory no longer exists, or the command wasn't found).
    pub(super) fn on_pending_command_completed_for_cli_agent_restore(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.cli_agent_session_restore.phase == RecoveryPhase::ResumePending {
            self.mark_cli_agent_session_interrupted(true, ctx);
        }
    }

    fn mark_cli_agent_session_interrupted(
        &mut self,
        previous_resume_failed: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume_acknowledgement_timer();
        if state.record.is_none() {
            state.phase = RecoveryPhase::Tracking;
            return;
        }
        state.phase = RecoveryPhase::Interrupted;
        state.record_is_live_instance = false;
        self.show_cli_agent_session_restore_banner(previous_resume_failed, ctx);
        ctx.notify();
    }

    fn clear_cli_agent_session_record(&mut self, ctx: &mut ViewContext<Self>) {
        let before = self.cli_agent_session_for_snapshot();
        let state = &mut self.cli_agent_session_restore;
        state.record = None;
        state.phase = RecoveryPhase::Tracking;
        state.record_is_live_instance = false;
        state.show_banner_after_bootstrap = false;
        state.cancel_resume_acknowledgement_timer();
        self.remove_cli_agent_session_restore_banner(ctx);
        ctx.notify();
        self.save_if_cli_agent_record_changed(before, ctx);
    }

    fn dismiss_restored_cli_agent_session(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_agent_session_restore.phase == RecoveryPhase::Interrupted {
            self.clear_cli_agent_session_record(ctx);
        } else {
            self.remove_cli_agent_session_restore_banner(ctx);
        }
    }

    fn save_if_cli_agent_record_changed(
        &mut self,
        before: Option<PersistedCLIAgentSession>,
        ctx: &mut ViewContext<Self>,
    ) {
        let after = self.cli_agent_session_restore.record.as_ref();
        if same_record(before.as_ref(), after) {
            return;
        }
        if ctx.has_singleton_model::<CLIAgentRestoreSaveScheduler>() {
            CLIAgentRestoreSaveScheduler::handle(ctx).update(ctx, |scheduler, ctx| {
                scheduler.request_save(ctx);
            });
        } else {
            ctx.dispatch_global_action("workspace:save_app", ());
        }
    }

    fn copy_restored_cli_agent_session_command(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(record) = self.cli_agent_session_restore.record.as_ref() else {
            return;
        };
        // A remote session's command runs on its host, whose shell is unknown here.
        let shell_type = if record.is_remote() {
            ShellType::Bash
        } else {
            self.active_session_shell_type(ctx)
                .unwrap_or(ShellType::Bash)
        };
        let Some(command) = record.resume_command_line(shell_type) else {
            return;
        };
        ctx.clipboard().write(ClipboardContent::plain_text(command));
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            let toast = DismissibleToast::default("Resume command copied".to_owned());
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        });
    }

    fn show_cli_agent_session_restore_banner(
        &mut self,
        previous_resume_failed: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.cli_agent_session_restore.phase != RecoveryPhase::Interrupted {
            return;
        }
        let Some(record) = self.cli_agent_session_restore.record.clone() else {
            return;
        };
        self.remove_cli_agent_session_restore_banner(ctx);

        let banner_record = record.clone();
        let banner = ctx.add_typed_action_view(move |_| {
            CLIAgentSessionRestoreBanner::new(banner_record, previous_resume_failed)
        });
        ctx.subscribe_to_view(&banner, |me, _, event, ctx| match event {
            CLIAgentSessionRestoreBannerEvent::Resume => {
                let message = match me.resume_restored_cli_agent_session(ctx) {
                    CLIAgentResumeOutcome::Busy => {
                        Some("Can't resume while something is running in this pane")
                    }
                    CLIAgentResumeOutcome::HasDraft => {
                        Some("Clear the input first: resuming would replace what you typed")
                    }
                    CLIAgentResumeOutcome::Resumed
                    | CLIAgentResumeOutcome::NothingToResume
                    | CLIAgentResumeOutcome::RemoteOnly => None,
                };
                if let Some(message) = message {
                    me.show_error_toast(message.to_owned(), ctx);
                }
            }
            CLIAgentSessionRestoreBannerEvent::CopyCommand => {
                me.copy_restored_cli_agent_session_command(ctx);
            }
            CLIAgentSessionRestoreBannerEvent::Dismiss => {
                me.dismiss_restored_cli_agent_session(ctx);
            }
        });
        self.insert_rich_content(
            None,
            banner.clone(),
            Some(RichContentMetadata::CLIAgentSessionRestoreBanner {
                handle: banner.clone(),
            }),
            RichContentInsertionPosition::Append {
                insert_below_long_running_block: true,
            },
            ctx,
        );
        self.cli_agent_session_restore.banner = Some(banner.clone());

        #[cfg(not(target_family = "wasm"))]
        if !record.is_remote() {
            use crate::terminal::cli_agent_sessions::restore::transcript_exists;

            let banner = banner.downgrade();
            ctx.spawn(
                async move { transcript_exists(&record) },
                move |_, exists, ctx| {
                    if exists == Some(false)
                        && let Some(banner) = banner.upgrade(ctx)
                    {
                        banner.update(ctx, |banner, ctx| {
                            banner.set_transcript_missing(true, ctx);
                        });
                    }
                },
            );
        }
    }

    fn remove_cli_agent_session_restore_banner(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(banner) = self.cli_agent_session_restore.banner.take() else {
            return;
        };
        let banner_view_id = banner.id();
        self.model
            .lock()
            .block_list_mut()
            .remove_rich_content(banner_view_id);
        self.rich_content_views
            .retain(|rich_content| rich_content.view_id() != banner_view_id);
        ctx.notify();
    }
}

#[cfg(test)]
#[path = "cli_agent_session_restore_tests.rs"]
mod tests;
