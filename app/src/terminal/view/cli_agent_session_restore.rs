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
//!   left by an agent that exited with an error. Once the login shell has bootstrapped, the pane
//!   offers to resume it with an inline banner, and the sidebar can show it. A new agent session
//!   with activity in the pane supersedes it; Dismiss clears it.
//! - **ResumeQueued**: a resume was requested and waits for the shell. If it can't run within
//!   [`RESUME_QUEUE_TIMEOUT`], the record goes back to Interrupted.
//! - **ResumePending**: the resume commands are being typed into the shell one at a time (a `cd`
//!   to the launch directory, then the agent), each only into an empty input. The record is
//!   kept, so another crash before the resumed agent reports back still has it. The resume is
//!   acknowledged once the agent reports the same session id. If a command fails, or nothing ran
//!   within [`RESUME_ACKNOWLEDGEMENT_TIMEOUT`] (a command still waiting in the input is then
//!   withdrawn), the record goes back to Interrupted. The agent command's block exiting
//!   successfully clears the record in any phase.

use std::collections::VecDeque;
use std::time::Duration;

use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::clipboard::ClipboardContent;
use warpui::{AppContext, SingletonEntity, ViewContext, ViewHandle};

use super::{EXECUTE_PENDING_COMMAND_DELAY, Event};
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
use crate::terminal::{CLIAgent, TerminalView};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

/// How long a requested resume may wait for the shell before it's offered again.
const RESUME_QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long sent resume commands may go without running or the agent starting before the
/// session is offered again. Re-armed while a resume command is still running.
const RESUME_ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RecoveryPhase {
    /// The record (if any) follows the agent session running in this pane.
    #[default]
    Tracking,
    /// The record describes a session that was interrupted and can be resumed here.
    Interrupted,
    /// Resume was requested before the shell was ready; it runs once the shell is.
    ResumeQueued,
    /// The resume commands are being sent; waiting for the agent to report the same session.
    ResumePending,
}

/// A command typed into the shell to resume a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeStep {
    ChangeDirectory,
    Agent,
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
    /// Whether this pane has acted on its login shell having bootstrapped.
    shell_ready_handled: bool,
    show_banner_after_bootstrap: bool,
    banner: Option<ViewHandle<CLIAgentSessionRestoreBanner>>,
    /// Resume commands that haven't been typed into the shell yet.
    pending_resume_steps: VecDeque<(ResumeStep, String)>,
    /// The resume command handed to the shell (waiting in the input or running), if any.
    in_flight_resume_step: Option<(ResumeStep, String)>,
    next_resume_step_timer: Option<SpawnedFutureHandle>,
    /// Times out a queued resume, or a sent one that was never acknowledged.
    resume_timer: Option<SpawnedFutureHandle>,
}

impl CLIAgentSessionRestoreState {
    fn cancel_resume(&mut self) {
        if let Some(timer) = self.resume_timer.take() {
            timer.abort();
        }
        if let Some(timer) = self.next_resume_step_timer.take() {
            timer.abort();
        }
        self.pending_resume_steps.clear();
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
    /// The session didn't start from this pane's local shell (e.g. it ran over SSH or in tmux);
    /// the pane can only show it and copy its command.
    CopyOnly,
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
    /// resumed in this pane, if any. Cheap; suitable for calling while rendering. Changes are
    /// announced with [`Event::TerminalViewStateChanged`]. Only a record for which
    /// [`PersistedCLIAgentSession::can_resume_here`] holds can be resumed by the pane; others
    /// are informational.
    pub(crate) fn restorable_cli_agent_session(&self) -> Option<&PersistedCLIAgentSession> {
        let state = &self.cli_agent_session_restore;
        state
            .record
            .as_ref()
            .filter(|_| state.phase == RecoveryPhase::Interrupted)
    }

    /// Moves to `phase`, letting the workspace (e.g. the sidebar) know when the pane starts or
    /// stops offering an interrupted session.
    fn set_cli_agent_recovery_phase(&mut self, phase: RecoveryPhase, ctx: &mut ViewContext<Self>) {
        let state = &mut self.cli_agent_session_restore;
        let was_interrupted = state.phase == RecoveryPhase::Interrupted;
        state.phase = phase;
        if was_interrupted != (phase == RecoveryPhase::Interrupted) {
            ctx.emit(Event::TerminalViewStateChanged);
        }
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
        let auto_resume = *GeneralSettings::as_ref(ctx).auto_resume_cli_agent_sessions
            && record.can_resume_here();
        self.remove_cli_agent_session_restore_banner(ctx);
        let was_interrupted = self.cli_agent_session_restore.phase == RecoveryPhase::Interrupted;
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume();
        state.in_flight_resume_step = None;
        state.record = Some(record);
        state.record_is_live_instance = false;
        if auto_resume {
            state.show_banner_after_bootstrap = false;
            self.queue_cli_agent_resume(ctx);
            return;
        }
        state.show_banner_after_bootstrap = true;
        self.set_cli_agent_recovery_phase(RecoveryPhase::Interrupted, ctx);
        if was_interrupted {
            // Still interrupted, but with another record.
            ctx.emit(Event::TerminalViewStateChanged);
        }
        if self.cli_agent_session_restore.shell_ready_handled {
            self.cli_agent_session_restore.show_banner_after_bootstrap = false;
            self.show_cli_agent_session_restore_banner(false, ctx);
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
        if !record.can_resume_here() {
            return CLIAgentResumeOutcome::CopyOnly;
        }
        if let Some(blocker) = self.cli_agent_resume_blocker(ctx) {
            return blocker;
        }

        self.remove_cli_agent_session_restore_banner(ctx);
        self.cli_agent_session_restore.show_banner_after_bootstrap = false;
        self.queue_cli_agent_resume(ctx);
        ctx.notify();
        CLIAgentResumeOutcome::Resumed
    }

    /// Why the resume commands can't be typed into this pane right now, if they can't.
    fn cli_agent_resume_blocker(&self, ctx: &AppContext) -> Option<CLIAgentResumeOutcome> {
        let busy = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .is_some()
            || self.has_pending_command_or_awaiting_completion(ctx)
            || self.is_long_running();
        if busy {
            return Some(CLIAgentResumeOutcome::Busy);
        }
        // The command is inserted into the editor, so it would mix with what's typed there.
        if !self.input.as_ref(ctx).buffer_text(ctx).is_empty() {
            return Some(CLIAgentResumeOutcome::HasDraft);
        }
        None
    }

    /// Queues the resume and sends it right away if the shell is ready.
    fn queue_cli_agent_resume(&mut self, ctx: &mut ViewContext<Self>) {
        self.cli_agent_session_restore.cancel_resume();
        self.set_cli_agent_recovery_phase(RecoveryPhase::ResumeQueued, ctx);
        self.arm_cli_agent_resume_timer(RESUME_QUEUE_TIMEOUT, ctx);
        self.try_send_queued_cli_agent_resume(ctx);
    }

    fn arm_cli_agent_resume_timer(&mut self, timeout: Duration, ctx: &mut ViewContext<Self>) {
        let timer = ctx.spawn(
            async move {
                Timer::after(timeout).await;
            },
            |me, _, ctx| me.on_cli_agent_resume_timeout(ctx),
        );
        if let Some(previous) = self.cli_agent_session_restore.resume_timer.replace(timer) {
            previous.abort();
        }
    }

    /// Sends a queued resume once the login shell is ready, or offers the session again if the
    /// pane became busy or has typed input meanwhile. Otherwise it keeps waiting, until the next
    /// block completes or the queue times out.
    fn try_send_queued_cli_agent_resume(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_agent_session_restore.phase != RecoveryPhase::ResumeQueued {
            return;
        }
        if !self.is_login_shell_bootstrapped || self.active_session_shell_type(ctx).is_none() {
            return;
        }
        if self.cli_agent_resume_blocker(ctx).is_some() {
            // Never type over a draft or into a busy pane; let the user decide.
            self.mark_cli_agent_session_interrupted(false, ctx);
            return;
        }
        self.send_cli_agent_resume_commands(ctx);
    }

    /// Starts typing the commands that resume the interrupted session into the shell.
    fn send_cli_agent_resume_commands(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(shell_type) = self.active_session_shell_type(ctx) else {
            return;
        };
        let pane_cwd = self.pwd();
        let Some(steps) = self
            .cli_agent_session_restore
            .record
            .as_ref()
            .and_then(|record| record.resume_command_steps(shell_type, pane_cwd.as_deref()))
        else {
            self.mark_cli_agent_session_interrupted(true, ctx);
            return;
        };
        // The agent command is always the last step.
        let agent_step = steps.len().saturating_sub(1);
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume();
        state.pending_resume_steps = steps
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                let step = if index == agent_step {
                    ResumeStep::Agent
                } else {
                    ResumeStep::ChangeDirectory
                };
                (step, command)
            })
            .collect();
        state.in_flight_resume_step = None;
        state.agent_block_active = false;
        self.set_cli_agent_recovery_phase(RecoveryPhase::ResumePending, ctx);
        self.arm_cli_agent_resume_timer(RESUME_ACKNOWLEDGEMENT_TIMEOUT, ctx);
        self.send_next_cli_agent_resume_step(ctx);
    }

    /// Types the next resume command into the shell, but only into an empty input of an idle
    /// pane: anything the user typed would be merged into the command. In that case the resume
    /// stops and the session is offered again.
    fn send_next_cli_agent_resume_step(&mut self, ctx: &mut ViewContext<Self>) {
        let state = &mut self.cli_agent_session_restore;
        state.next_resume_step_timer = None;
        if state.phase != RecoveryPhase::ResumePending || state.in_flight_resume_step.is_some() {
            return;
        }
        let Some((step, command)) = state.pending_resume_steps.pop_front() else {
            return;
        };
        if self.cli_agent_resume_blocker(ctx).is_some() {
            self.mark_cli_agent_session_interrupted(true, ctx);
            return;
        }
        self.set_pending_command(&command, ctx);
        self.cli_agent_session_restore.in_flight_resume_step = Some((step, command));
        self.execute_pending_command((), ctx);
    }

    fn on_cli_agent_resume_timeout(&mut self, ctx: &mut ViewContext<Self>) {
        self.cli_agent_session_restore.resume_timer = None;
        match self.cli_agent_session_restore.phase {
            RecoveryPhase::ResumeQueued => {
                log::info!("A queued CLI agent session resume never got to run");
                self.mark_cli_agent_session_interrupted(true, ctx);
            }
            RecoveryPhase::ResumePending => {
                let state = &self.cli_agent_session_restore;
                // A resume command that's still running (e.g. an agent that hasn't reported its
                // session yet) is settled by its block finishing instead.
                let resume_command_running = state.in_flight_resume_step.is_some()
                    && self.awaiting_pending_command_completion;
                let still_resuming = resume_command_running
                    || state.next_resume_step_timer.is_some()
                    || CLIAgentSessionsModel::as_ref(ctx)
                        .session(self.view_id)
                        .is_some();
                if still_resuming {
                    self.arm_cli_agent_resume_timer(RESUME_ACKNOWLEDGEMENT_TIMEOUT, ctx);
                } else {
                    log::info!("CLI agent session resume commands never ran");
                    self.withdraw_unsent_cli_agent_resume_command(ctx);
                    self.mark_cli_agent_session_interrupted(true, ctx);
                }
            }
            RecoveryPhase::Tracking | RecoveryPhase::Interrupted => {}
        }
    }

    /// Takes back a resume command that still waits in the input for the shell, so a later
    /// prompt doesn't run it (with whatever was typed after it) once the session is offered
    /// again. The command is removed from the editor unless the user has edited it.
    fn withdraw_unsent_cli_agent_resume_command(&mut self, ctx: &mut ViewContext<Self>) {
        if self.awaiting_pending_command_completion {
            // It was handed to the shell; its block settles the resume.
            return;
        }
        let Some((_, command)) = self.cli_agent_session_restore.in_flight_resume_step.take() else {
            return;
        };
        self.input.update(ctx, |input, ctx| {
            if input.cancel_pending_command() && input.buffer_text(ctx) == command {
                input.replace_buffer_content("", ctx);
            }
        });
    }

    /// Shows the restore banner once the login shell has bootstrapped. Which bootstrap event
    /// arrives first varies, so this is called from several; only the first call acts.
    pub(super) fn on_login_shell_bootstrapped_for_cli_agent_restore(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        if !self.is_login_shell_bootstrapped {
            return;
        }
        let state = &mut self.cli_agent_session_restore;
        if std::mem::replace(&mut state.shell_ready_handled, true) {
            return;
        }
        let show_banner = std::mem::take(&mut state.show_banner_after_bootstrap);
        if show_banner && state.phase == RecoveryPhase::Interrupted {
            self.show_cli_agent_session_restore_banner(false, ctx);
        }
    }

    /// Called when the shell reports its prompt without a completed block
    /// (`BootstrapPrecmdDone`): runs a queued resume right away, as pending commands are.
    pub(super) fn on_shell_prompt_for_cli_agent_restore(&mut self, ctx: &mut ViewContext<Self>) {
        self.on_login_shell_bootstrapped_for_cli_agent_restore(ctx);
        self.try_send_queued_cli_agent_resume(ctx);
    }

    /// Called after a block completes in a bootstrapped shell, including the bootstrap blocks
    /// that precede the first prompt. Runs a queued resume after the same short delay as pending
    /// commands, since the shell may still be running prompt hooks.
    pub(super) fn on_block_completed_for_cli_agent_restore(&mut self, ctx: &mut ViewContext<Self>) {
        self.on_login_shell_bootstrapped_for_cli_agent_restore(ctx);
        if self.cli_agent_session_restore.phase != RecoveryPhase::ResumeQueued {
            return;
        }
        let _ = ctx.spawn(
            async {
                Timer::after(EXECUTE_PENDING_COMMAND_DELAY).await;
            },
            |me, _, ctx| me.try_send_queued_cli_agent_resume(ctx),
        );
    }

    /// Whether the command running in this pane is `agent`, started from the pane's local login
    /// shell (directly or through an alias Warp resolves), so the session can be resumed by
    /// typing its command into a fresh local shell.
    fn running_command_is_local_cli_agent(&self, agent: CLIAgent, ctx: &AppContext) -> bool {
        let Some(session_id) = self.active_block_session_id() else {
            return false;
        };
        let is_local_login_shell = self.session_is_local(session_id, ctx)
            && self
                .sessions
                .as_ref(ctx)
                .get(session_id)
                .is_some_and(|session| session.subshell_info().is_none());
        if !is_local_login_shell {
            return false;
        }
        let detected = {
            let model = self.model.lock();
            self.detect_cli_agent_from_model(&model, ctx)
        };
        // Custom toolbar command patterns aren't trusted: they can match wrappers (e.g. ones that
        // ssh somewhere first).
        matches!(detected, Some((detected, None)) if detected == agent)
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
        // Read the model rather than the event: events are delivered after the fact, and the
        // model has the latest state (e.g. the replacement after a session was replaced).
        let app: &AppContext = ctx;
        let launched_locally = CLIAgentSessionsModel::as_ref(app)
            .session(self.view_id)
            .is_some_and(|session| self.running_command_is_local_cli_agent(session.agent, app));
        let state = &mut self.cli_agent_session_restore;
        if matches!(event, CLIAgentSessionsModelEvent::Ended { .. }) {
            // Whatever is registered now (if anything) is a different session instance. Ending
            // alone isn't a deliberate exit, so the record stays until the block finishes.
            state.record_is_live_instance = false;
        }

        let mut next_phase = None;
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
                        launched_locally,
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
                        launched_locally,
                        now,
                    ) {
                        state.record = Some(live);
                        state.record_is_live_instance = true;
                        state.show_banner_after_bootstrap = false;
                        state.cancel_resume();
                        next_phase = Some(RecoveryPhase::Tracking);
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
                        launched_locally,
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
                        if let Some(timer) = state.resume_timer.take() {
                            timer.abort();
                        }
                        next_phase = Some(RecoveryPhase::Tracking);
                    }
                }
            }
        }

        if let Some(phase) = next_phase {
            let superseded = self.cli_agent_session_restore.phase != RecoveryPhase::ResumePending;
            self.set_cli_agent_recovery_phase(phase, ctx);
            if superseded {
                self.remove_cli_agent_session_restore_banner(ctx);
            }
        }
        self.save_if_cli_agent_record_changed(before, ctx);
    }

    /// Settles the record when a user command block finishes: a resume command moves the resume
    /// along, and a block that ran an agent session is either a deliberate quit (exit 0) or
    /// leaves the session interrupted.
    pub(super) fn on_user_block_completed_for_cli_agent_restore(
        &mut self,
        succeeded: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        // A pending command completing is the resume command this pane typed, since a resume is
        // only typed into an idle pane.
        let pending_command_completed = self.awaiting_pending_command_completion;
        let state = &mut self.cli_agent_session_restore;
        let agent_block_was_active = std::mem::take(&mut state.agent_block_active);
        let finished_step = if pending_command_completed {
            state.in_flight_resume_step.take().map(|(step, _)| step)
        } else {
            None
        };
        match finished_step {
            Some(ResumeStep::ChangeDirectory) => {
                if succeeded
                    && state.phase == RecoveryPhase::ResumePending
                    && !state.pending_resume_steps.is_empty()
                {
                    // Like queued pending commands, give the shell a moment after its prompt.
                    let timer = ctx.spawn(
                        async {
                            Timer::after(EXECUTE_PENDING_COMMAND_DELAY).await;
                        },
                        |me, _, ctx| me.send_next_cli_agent_resume_step(ctx),
                    );
                    state.next_resume_step_timer = Some(timer);
                } else if state.phase == RecoveryPhase::ResumePending {
                    // e.g. the launch directory no longer exists.
                    self.mark_cli_agent_session_interrupted(true, ctx);
                }
                return;
            }
            Some(ResumeStep::Agent) => {
                state.record_is_live_instance = false;
                let was_resuming = state.phase != RecoveryPhase::Tracking;
                if state.record.is_none() {
                    return;
                }
                if succeeded {
                    // The resumed agent was quit deliberately, even if the pane had already
                    // given up waiting for it to report back.
                    self.clear_cli_agent_session_record(ctx);
                } else {
                    self.mark_cli_agent_session_interrupted(was_resuming, ctx);
                }
                return;
            }
            None => {}
        }

        if !agent_block_was_active {
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

    fn mark_cli_agent_session_interrupted(
        &mut self,
        previous_resume_failed: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume();
        if state.record.is_none() {
            self.set_cli_agent_recovery_phase(RecoveryPhase::Tracking, ctx);
            return;
        }
        state.record_is_live_instance = false;
        self.set_cli_agent_recovery_phase(RecoveryPhase::Interrupted, ctx);
        self.show_cli_agent_session_restore_banner(previous_resume_failed, ctx);
        ctx.notify();
    }

    fn clear_cli_agent_session_record(&mut self, ctx: &mut ViewContext<Self>) {
        let before = self.cli_agent_session_for_snapshot();
        let state = &mut self.cli_agent_session_restore;
        state.record = None;
        state.record_is_live_instance = false;
        state.show_banner_after_bootstrap = false;
        state.in_flight_resume_step = None;
        state.cancel_resume();
        self.set_cli_agent_recovery_phase(RecoveryPhase::Tracking, ctx);
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
        // A session that didn't start from this pane's local shell is resumed wherever it ran,
        // whose shell is unknown here.
        let shell_type = if record.can_resume_here() {
            self.active_session_shell_type(ctx)
                .unwrap_or(ShellType::Bash)
        } else {
            ShellType::Bash
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
                    | CLIAgentResumeOutcome::CopyOnly => None,
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
        if record.can_resume_here() {
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
