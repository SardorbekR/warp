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
//!   [`RESUME_QUEUE_TIMEOUT`] of the shell bootstrapping (or the shell doesn't bootstrap within
//!   [`RESUME_BOOTSTRAP_FALLBACK`]), the record goes back to Interrupted.
//! - **ResumePending**: the resume commands are being typed into the shell one at a time (a `cd`
//!   to the launch directory, then the agent), each only into an empty input. The record is
//!   kept, so another crash before the resumed agent reports back still has it. The resume is
//!   acknowledged once the agent reports the same session id. If a command fails, or nothing ran
//!   within [`RESUME_ACKNOWLEDGEMENT_TIMEOUT`] (a command still waiting in the input is then
//!   withdrawn), the record goes back to Interrupted. The agent command's block exiting
//!   successfully once the resume was acknowledged clears the record.
//!
//! While a pane resumes or tracks a session, it claims the session in [`CLIAgentSessionsModel`]
//! so no other pane resumes it too, and other panes stop offering it once it's tracked.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Duration;

use instant::Instant;
use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::clipboard::ClipboardContent;
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext, ViewHandle};

use super::{EXECUTE_PENDING_COMMAND_DELAY, Event};
use crate::ai::agent_management::AgentNotificationsModel;
use crate::settings::CompiledCommandsForCodingAgentToolbar;
use crate::terminal::cli_agent_sessions::launch_command::runs_through_remote_wrapper;
use crate::terminal::cli_agent_sessions::restore::{
    LaunchObservation, PersistedCLIAgentSession, SessionCommand, is_resumable_agent,
    reported_session_id,
};
use crate::terminal::cli_agent_sessions::restore_save::CLIAgentRestoreSaveScheduler;
use crate::terminal::cli_agent_sessions::session_name::{
    session_name_from_prompt, session_name_from_terminal_title,
};
use crate::terminal::cli_agent_sessions::{
    CLIAgentResumeClaim, CLIAgentResumeClaimKind, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::shell::ShellType;
use crate::terminal::view::cli_agent_session_restore_banner::{
    CLIAgentSessionRestoreBanner, CLIAgentSessionRestoreBannerEvent, PreviousResumeAttempt,
};
use crate::terminal::view::{RichContentInsertionPosition, RichContentMetadata};
use crate::terminal::{CLIAgent, TerminalView};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

/// How long a requested resume may wait for the shell, once it has bootstrapped, before it's
/// offered again.
const RESUME_QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long sent resume commands may go without running or the agent starting before the
/// session is offered again. Re-armed while a resume command is still running.
const RESUME_ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a resume queued before the login shell bootstrapped waits for it before the session
/// is offered again, e.g. when the shell is stuck in its startup files.
const RESUME_BOOTSTRAP_FALLBACK: Duration = Duration::from_secs(120);
/// Fork: how far apart queued resumes start. Otherwise a restored window full of agent tabs
/// resumes them all at once, each agent loading its conversation and starting its tools.
const RESUME_SPACING: Duration = Duration::from_secs(2);

thread_local! {
    /// Fork: the earliest time the next queued resume may start, shared by all panes (which live
    /// on the main thread).
    static NEXT_RESUME_START: Cell<Option<Instant>> = const { Cell::new(None) };
}

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

/// Whether the record's launch directory exists on this machine. Checked off the main thread; a
/// directory that doesn't exist here means the agent ran somewhere else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum LaunchDirectoryCheck {
    #[default]
    NotChecked,
    Checking(String),
    Checked {
        launch_cwd: String,
        exists: bool,
    },
}

/// Fork: whether the conversation (transcript) of a record saved before its session did
/// anything exists on this machine, which decides whether the session is resumed or started
/// anew (see [`PersistedCLIAgentSession::session_command`]). Checked off the main thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum ConversationCheck {
    #[default]
    NotChecked,
    /// Checking the session with this id.
    Checking(String),
    Checked {
        session_id: String,
        exists: Option<bool>,
    },
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
    /// Whether the banner waits for the login shell to bootstrap, so it isn't inserted among
    /// the bootstrap output.
    show_banner_after_bootstrap: bool,
    banner: Option<ViewHandle<CLIAgentSessionRestoreBanner>>,
    /// What the banner says about the last resume attempt, kept for when it's shown again.
    previous_resume: PreviousResumeAttempt,
    launch_directory: LaunchDirectoryCheck,
    /// Fork: see [`ConversationCheck`].
    conversation: ConversationCheck,
    /// Fork: whether the commands being sent start a new session instead of resuming the
    /// record's (see [`SessionCommand::StartNew`]).
    resume_starts_new_session: bool,
    /// Resume commands that haven't been typed into the shell yet.
    pending_resume_steps: VecDeque<(ResumeStep, String)>,
    /// The resume command handed to the shell (waiting in the input or running), if any.
    in_flight_resume_step: Option<(ResumeStep, String)>,
    /// Whether the agent started by the resume reported the restored session (or moved on to
    /// another one), which confirms the resume worked.
    resume_acknowledged: bool,
    next_resume_step_timer: Option<SpawnedFutureHandle>,
    /// Times out a queued resume, or a sent one that was never acknowledged.
    resume_timer: Option<SpawnedFutureHandle>,
    /// When a resume queued before the login shell bootstrapped gives up waiting for it.
    bootstrap_fallback_deadline: Option<Instant>,
    /// Fork: when this pane's queued resume may start (see [`RESUME_SPACING`]), once it has
    /// claimed its turn.
    resume_start: Option<Instant>,
    /// Fork: tries the queued resume again once its turn has come.
    resume_start_timer: Option<SpawnedFutureHandle>,
    /// The claim last registered with [`CLIAgentSessionsModel`] for this pane.
    resume_claim: Option<CLIAgentResumeClaim>,
}

impl CLIAgentSessionRestoreState {
    fn cancel_resume(&mut self) {
        if let Some(timer) = self.resume_timer.take() {
            timer.abort();
        }
        if let Some(timer) = self.next_resume_step_timer.take() {
            timer.abort();
        }
        if let Some(timer) = self.resume_start_timer.take() {
            timer.abort();
        }
        self.pending_resume_steps.clear();
    }
}

/// Fork: when a queued resume claiming its turn at `now` may start, given the earliest time
/// the next one may (`next`), and the new earliest time for the one after it. The focused pane
/// starts right away; the others take turns [`RESUME_SPACING`] apart.
fn claim_resume_start(next: Option<Instant>, now: Instant, focused: bool) -> (Instant, Instant) {
    let next = next.filter(|next| *next > now).unwrap_or(now);
    let start = if focused { now } else { next };
    let after = start.checked_add(RESUME_SPACING).unwrap_or(start);
    (start, next.max(after))
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
    /// The session is already open in another pane, so resuming it here would run it twice.
    AlreadyOpen,
    /// The session didn't start from this pane's local shell (e.g. it ran over SSH or in tmux),
    /// or its directory doesn't exist here; the pane can only show it and copy its command.
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

    /// [`Self::cli_agent_session_for_snapshot`] with when the session last changed status and
    /// (fork) the session's name, for the saved app state.
    pub(crate) fn cli_agent_session_for_app_state(
        &self,
        app: &AppContext,
    ) -> Option<PersistedCLIAgentSession> {
        let mut record = self.cli_agent_session_for_snapshot()?;
        if app.has_singleton_model::<AgentNotificationsModel>()
            && let Some(at) =
                AgentNotificationsModel::as_ref(app).last_activity_unix_ms(self.view_id)
        {
            record.last_activity_at_unix_ms = Some(at);
        }
        if let Some(name) = self.tracked_cli_agent_session_name(app) {
            record.session_name = Some(name);
        }
        Some(record)
    }

    /// Fork: the name of the CLI agent session running in this pane (see
    /// [`crate::terminal::cli_agent_sessions::session_name`]): the one the agent puts in the
    /// terminal title, else the one saved for the session, else the session's first prompt.
    /// Cheap; suitable for calling while rendering.
    pub(crate) fn live_cli_agent_session_name(&self, app: &AppContext) -> Option<String> {
        if !app.has_singleton_model::<CLIAgentSessionsModel>() {
            return None;
        }
        let session = CLIAgentSessionsModel::as_ref(app).session(self.view_id)?;
        session_name_from_terminal_title(session.agent, &self.terminal_title)
            .or_else(|| {
                let record = self.cli_agent_session_restore.record.as_ref()?;
                let is_same_session = record.agent == session.agent
                    && reported_session_id(session).is_some()
                    && record.session_id.as_deref() == reported_session_id(session);
                is_same_session
                    .then(|| record.session_name.clone())
                    .flatten()
            })
            .or_else(|| {
                session
                    .session_context
                    .first_query
                    .as_deref()
                    .and_then(session_name_from_prompt)
            })
    }

    /// Fork: [`Self::live_cli_agent_session_name`] when the running session is the one the
    /// record follows, which is the only time the record may take its name.
    fn tracked_cli_agent_session_name(&self, app: &AppContext) -> Option<String> {
        let state = &self.cli_agent_session_restore;
        if state.phase != RecoveryPhase::Tracking || !state.record_is_live_instance {
            return None;
        }
        let record = state.record.as_ref()?;
        if !app.has_singleton_model::<CLIAgentSessionsModel>() {
            return None;
        }
        let session = CLIAgentSessionsModel::as_ref(app).session(self.view_id)?;
        let is_same_session = session.agent == record.agent
            && reported_session_id(session).is_some()
            && reported_session_id(session) == record.session_id.as_deref();
        is_same_session
            .then(|| self.live_cli_agent_session_name(app))
            .flatten()
    }

    /// Fork: the terminal title may name the tracked session (Claude Code puts its name there).
    pub(super) fn on_terminal_title_changed_for_cli_agent_restore(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        self.refresh_cli_agent_session_name(ctx);
    }

    /// Fork: keeps the record's name in step with the tracked session's, so that it outlives
    /// the session (e.g. an agent that failed leaves the pane offering the session by its
    /// name). A new name asks for a lazy app-state save, so that the pane restored after a
    /// crash shows it. The title also changes while the agent works (its status glyph), which
    /// doesn't change the name.
    fn refresh_cli_agent_session_name(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(name) = self.tracked_cli_agent_session_name(ctx) else {
            return;
        };
        let Some(record) = self.cli_agent_session_restore.record.as_mut() else {
            return;
        };
        if record.session_name.as_ref() == Some(&name) {
            return;
        }
        record.session_name = Some(name);
        if ctx.has_singleton_model::<CLIAgentRestoreSaveScheduler>() {
            CLIAgentRestoreSaveScheduler::handle(ctx).update(ctx, |scheduler, ctx| {
                scheduler.request_lazy_save(ctx);
            });
        }
    }

    /// The interrupted CLI agent session (e.g. restored after Warp quit or crashed) that this
    /// pane offers, if any. Cheap; suitable for calling while rendering. Changes are announced
    /// with [`Event::TerminalViewStateChanged`]. Whether it can be resumed in this pane, rather
    /// than only copied, is [`Self::can_resume_restored_cli_agent_session_here`].
    pub(crate) fn restorable_cli_agent_session(&self) -> Option<&PersistedCLIAgentSession> {
        let state = &self.cli_agent_session_restore;
        state
            .record
            .as_ref()
            .filter(|_| state.phase == RecoveryPhase::Interrupted)
    }

    /// Whether the offered session can be resumed in this pane, rather than only copied: it
    /// started from the pane's local shell and its directory isn't known to be missing here.
    pub(crate) fn can_resume_restored_cli_agent_session_here(&self) -> bool {
        self.cli_agent_session_restore
            .record
            .as_ref()
            .is_some_and(PersistedCLIAgentSession::can_resume_here)
            && self.cli_agent_launch_directory_exists() != Some(false)
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
        self.sync_cli_agent_resume_claim(ctx);
    }

    /// Registers the session this pane resumes or tracks with [`CLIAgentSessionsModel`], so other
    /// panes don't resume it too.
    fn sync_cli_agent_resume_claim(&mut self, ctx: &mut ViewContext<Self>) {
        let state = &self.cli_agent_session_restore;
        let kind = match state.phase {
            RecoveryPhase::Tracking => Some(CLIAgentResumeClaimKind::Tracking),
            RecoveryPhase::ResumeQueued | RecoveryPhase::ResumePending => {
                Some(CLIAgentResumeClaimKind::Resuming)
            }
            RecoveryPhase::Interrupted => None,
        };
        let claim = kind.and_then(|kind| {
            let record = state.record.as_ref()?;
            Some(CLIAgentResumeClaim {
                agent: record.agent,
                session_id: record.session_id.clone()?,
                kind,
            })
        });
        if state.resume_claim == claim {
            return;
        }
        self.cli_agent_session_restore.resume_claim = claim.clone();
        let pane = ctx.handle();
        CLIAgentSessionsModel::handle(ctx).update(ctx, |sessions, ctx| {
            sessions.set_resume_claim(pane, claim, ctx);
        });
    }

    /// The agent and session id of the session this pane offers or is about to resume.
    fn offered_cli_agent_session_key(&self) -> Option<(CLIAgent, String)> {
        let state = &self.cli_agent_session_restore;
        if !matches!(
            state.phase,
            RecoveryPhase::Interrupted | RecoveryPhase::ResumeQueued
        ) {
            return None;
        }
        let record = state.record.as_ref()?;
        Some((record.agent, record.session_id.clone()?))
    }

    /// After an event about another pane's agent session, checks whether that pane now tracks
    /// the session this pane offers. The check waits until every pane has handled the event, so
    /// the other pane's claim is up to date.
    fn schedule_cli_agent_offer_check(
        &mut self,
        other_view_id: EntityId,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some((agent, session_id)) = self.offered_cli_agent_session_key() else {
            return;
        };
        let other_runs_it = CLIAgentSessionsModel::as_ref(ctx)
            .session(other_view_id)
            .is_some_and(|session| {
                session.agent == agent
                    && session.session_context.resume_session_id.as_deref()
                        == Some(session_id.as_str())
            });
        if other_runs_it {
            let _ = ctx.spawn(async {}, |me, _, ctx| {
                me.drop_cli_agent_offer_tracked_elsewhere(ctx);
            });
        }
    }

    /// Stops offering (and forgets) the session when another pane tracks it, since that pane
    /// persists it from now on: two panes offering it could both resume it after a crash.
    fn drop_cli_agent_offer_tracked_elsewhere(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((agent, session_id)) = self.offered_cli_agent_session_key() else {
            return;
        };
        let tracked_elsewhere = CLIAgentSessionsModel::as_ref(ctx).resume_claim_elsewhere(
            self.view_id,
            agent,
            &session_id,
            ctx,
        ) == Some(CLIAgentResumeClaimKind::Tracking);
        if tracked_elsewhere {
            log::info!("An offered CLI agent session is now running in another pane");
            self.clear_cli_agent_session_record(ctx);
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
        // A headless run (e.g. `claude -p` in a script) is only resumed when asked to.
        let auto_resume = *GeneralSettings::as_ref(ctx).auto_resume_cli_agent_sessions
            && record.can_resume_here()
            && record.evidently_interactive;
        self.remove_cli_agent_session_restore_banner(ctx);
        // Keep the session's age and position in the status-ordered tabs panel.
        if let Some(at) = record.last_activity_at_unix_ms
            && ctx.has_singleton_model::<AgentNotificationsModel>()
        {
            let view_id = self.view_id;
            AgentNotificationsModel::handle(ctx).update(ctx, |model, ctx| {
                model.seed_session_activity(view_id, at, ctx);
            });
        }
        let was_interrupted = self.cli_agent_session_restore.phase == RecoveryPhase::Interrupted;
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume();
        state.in_flight_resume_step = None;
        state.record = Some(record);
        state.record_is_live_instance = false;
        state.resume_acknowledged = false;
        state.show_banner_after_bootstrap = false;
        state.launch_directory = LaunchDirectoryCheck::NotChecked;
        state.conversation = ConversationCheck::NotChecked;
        state.resume_starts_new_session = false;
        self.check_cli_agent_launch_directory(ctx);
        self.check_cli_agent_conversation(ctx);
        if auto_resume {
            self.queue_cli_agent_resume(ctx);
            return;
        }
        self.set_cli_agent_recovery_phase(RecoveryPhase::Interrupted, ctx);
        if was_interrupted {
            // Still interrupted, but with another record.
            ctx.emit(Event::TerminalViewStateChanged);
        }
        self.show_cli_agent_session_restore_banner(PreviousResumeAttempt::NotAttempted, ctx);
    }

    /// Resumes the interrupted CLI agent session in this pane, unless that could disturb
    /// something: a running or pending command, text the user typed into the input, or the same
    /// session open in another pane. Before the shell has bootstrapped, the resume is queued and
    /// these are checked again once it has.
    pub(crate) fn resume_restored_cli_agent_session(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) -> CLIAgentResumeOutcome {
        let state = &self.cli_agent_session_restore;
        if state.record.is_none() || state.phase != RecoveryPhase::Interrupted {
            return CLIAgentResumeOutcome::NothingToResume;
        }
        if !self.can_resume_restored_cli_agent_session_here() {
            return CLIAgentResumeOutcome::CopyOnly;
        }
        match self.cli_agent_resume_blocker(ctx) {
            // The bootstrap itself (or a setup command) makes the pane look busy until the shell
            // is ready; the queued resume waits for it.
            Some(CLIAgentResumeOutcome::Busy)
                if !self.cli_agent_session_restore.shell_ready_handled => {}
            Some(blocker) => return blocker,
            None => {}
        }

        self.remove_cli_agent_session_restore_banner(ctx);
        self.cli_agent_session_restore.show_banner_after_bootstrap = false;
        self.queue_cli_agent_resume(ctx);
        ctx.notify();
        CLIAgentResumeOutcome::Resumed
    }

    /// Resumes the offered session like [`Self::resume_restored_cli_agent_session`], telling the
    /// user with a toast when it can't.
    pub(crate) fn resume_restored_cli_agent_session_with_feedback(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        let message = match self.resume_restored_cli_agent_session(ctx) {
            CLIAgentResumeOutcome::Busy => {
                Some("Can't resume while something is running in this pane")
            }
            CLIAgentResumeOutcome::HasDraft => {
                Some("Clear the input first: resuming would replace what you typed")
            }
            CLIAgentResumeOutcome::AlreadyOpen => {
                Some("This session is already open in another pane")
            }
            CLIAgentResumeOutcome::CopyOnly => Some(
                "This session can't be resumed here; copy its command to resume it where it ran",
            ),
            CLIAgentResumeOutcome::Resumed | CLIAgentResumeOutcome::NothingToResume => None,
        };
        if let Some(message) = message {
            self.show_error_toast(message.to_owned(), ctx);
        }
    }

    /// Whether the session is running in, being resumed in, or tracked by another pane.
    fn cli_agent_session_open_elsewhere(&self, ctx: &AppContext) -> bool {
        let sessions = CLIAgentSessionsModel::as_ref(ctx);
        let Some(record) = self.cli_agent_session_restore.record.as_ref() else {
            return false;
        };
        record.session_id.as_deref().is_some_and(|session_id| {
            sessions.is_session_open_elsewhere(self.view_id, record.agent, session_id)
                || sessions
                    .resume_claim_elsewhere(self.view_id, record.agent, session_id, ctx)
                    .is_some()
        })
    }

    /// Why the resume commands can't be typed into this pane right now, if they can't.
    fn cli_agent_resume_blocker(&self, ctx: &AppContext) -> Option<CLIAgentResumeOutcome> {
        if self.cli_agent_session_open_elsewhere(ctx) {
            return Some(CLIAgentResumeOutcome::AlreadyOpen);
        }
        let sessions = CLIAgentSessionsModel::as_ref(ctx);
        let busy = sessions.session(self.view_id).is_some()
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

    /// Queues the resume and sends it right away if the shell is ready. A session that another
    /// pane resumes or runs is offered instead.
    fn queue_cli_agent_resume(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_agent_session_open_elsewhere(ctx) {
            self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::NotAttempted, ctx);
            return;
        }
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume();
        state.resume_acknowledged = false;
        state.resume_start = None;
        self.set_cli_agent_recovery_phase(RecoveryPhase::ResumeQueued, ctx);
        // The queue timeout starts once the shell has bootstrapped: a slow bootstrap isn't a
        // failure, though one that never finishes is.
        if self.cli_agent_session_restore.shell_ready_handled {
            self.arm_cli_agent_resume_timer(RESUME_QUEUE_TIMEOUT, ctx);
        } else {
            self.cli_agent_session_restore.bootstrap_fallback_deadline =
                Instant::now().checked_add(RESUME_BOOTSTRAP_FALLBACK);
            self.arm_cli_agent_resume_timer(RESUME_BOOTSTRAP_FALLBACK, ctx);
        }
        self.check_cli_agent_launch_directory(ctx);
        self.check_cli_agent_conversation(ctx);
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

    /// Tries to send a queued resume after the same short delay as pending commands, since the
    /// shell may still be running prompt hooks.
    fn schedule_queued_cli_agent_resume(&mut self, ctx: &mut ViewContext<Self>) {
        let _ = ctx.spawn(
            async {
                Timer::after(EXECUTE_PENDING_COMMAND_DELAY).await;
            },
            |me, _, ctx| me.try_send_queued_cli_agent_resume(ctx),
        );
    }

    /// Sends a queued resume once the login shell is ready and the launch directory was found.
    /// While the pane is busy it keeps waiting, for the next block to complete or the queue to
    /// time out; anything else that blocks it offers the session again.
    fn try_send_queued_cli_agent_resume(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_agent_session_restore.phase != RecoveryPhase::ResumeQueued {
            return;
        }
        if !self.is_login_shell_bootstrapped || self.active_session_shell_type(ctx).is_none() {
            return;
        }
        match self.cli_agent_launch_directory_exists() {
            // Checked off the main thread; the result tries again.
            None => {
                self.check_cli_agent_launch_directory(ctx);
                return;
            }
            Some(false) => {
                self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::NotAttempted, ctx);
                return;
            }
            Some(true) => {}
        }
        if self.cli_agent_session_command().is_none() {
            // Checked off the main thread; the result tries again.
            self.check_cli_agent_conversation(ctx);
            return;
        }
        match self.cli_agent_resume_blocker(ctx) {
            None => {
                if let Some(wait) = self.wait_for_cli_agent_resume_turn(ctx) {
                    self.schedule_cli_agent_resume_turn(wait, ctx);
                    return;
                }
                self.send_cli_agent_resume_commands(ctx);
            }
            Some(CLIAgentResumeOutcome::Busy) => {}
            // Never type over a draft or run a session twice; let the user decide.
            Some(
                CLIAgentResumeOutcome::HasDraft
                | CLIAgentResumeOutcome::AlreadyOpen
                | CLIAgentResumeOutcome::CopyOnly
                | CLIAgentResumeOutcome::NothingToResume
                | CLIAgentResumeOutcome::Resumed,
            ) => {
                self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::NotAttempted, ctx);
            }
        }
    }

    /// Fork: claims this pane's turn to start its queued resume, if it hasn't yet, and returns
    /// how long until the turn comes, if it hasn't come yet.
    fn wait_for_cli_agent_resume_turn(&mut self, ctx: &ViewContext<Self>) -> Option<Duration> {
        let now = Instant::now();
        let focused = ctx.is_self_or_child_focused();
        let start = *self
            .cli_agent_session_restore
            .resume_start
            .get_or_insert_with(|| {
                NEXT_RESUME_START.with(|next_start| {
                    let (start, next) = claim_resume_start(next_start.get(), now, focused);
                    next_start.set(Some(next));
                    start
                })
            });
        let wait = start.saturating_duration_since(now);
        (!wait.is_zero()).then_some(wait)
    }

    /// Fork: tries the queued resume again once its turn has come. The wait doesn't count
    /// against the queue timeout.
    fn schedule_cli_agent_resume_turn(&mut self, wait: Duration, ctx: &mut ViewContext<Self>) {
        if self.cli_agent_session_restore.resume_start_timer.is_some() {
            return;
        }
        self.arm_cli_agent_resume_timer(wait.saturating_add(RESUME_QUEUE_TIMEOUT), ctx);
        let timer = ctx.spawn(
            async move {
                Timer::after(wait).await;
            },
            |me, _, ctx| {
                me.cli_agent_session_restore.resume_start_timer = None;
                me.try_send_queued_cli_agent_resume(ctx);
            },
        );
        self.cli_agent_session_restore.resume_start_timer = Some(timer);
    }

    /// Starts typing the commands that resume the interrupted session into the shell.
    fn send_cli_agent_resume_commands(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(shell_type) = self.active_session_shell_type(ctx) else {
            return;
        };
        let pane_cwd = self.pwd();
        let command = self
            .cli_agent_session_command()
            .unwrap_or(SessionCommand::Resume);
        let Some(steps) = self
            .cli_agent_session_restore
            .record
            .as_ref()
            .and_then(|record| record.command_steps(command, shell_type, pane_cwd.as_deref()))
        else {
            self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx);
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
        state.resume_acknowledged = false;
        state.resume_starts_new_session = command == SessionCommand::StartNew;
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
            self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx);
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
                let state = &self.cli_agent_session_restore;
                if !state.shell_ready_handled {
                    // The queue timeout starts once the shell has bootstrapped, unless it never
                    // does.
                    let remaining = state
                        .bootstrap_fallback_deadline
                        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                        .unwrap_or_default();
                    if !remaining.is_zero() {
                        self.arm_cli_agent_resume_timer(remaining, ctx);
                        return;
                    }
                    log::info!("The shell didn't get ready for a queued CLI agent session resume");
                    self.mark_cli_agent_session_interrupted(
                        PreviousResumeAttempt::NotAttempted,
                        ctx,
                    );
                    return;
                }
                log::info!("A queued CLI agent session resume never got to run");
                self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx);
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
                    self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx);
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

    /// Whether the record's launch directory exists on this machine: `None` until checked. A
    /// record without one is resumed in the pane's directory.
    fn cli_agent_launch_directory_exists(&self) -> Option<bool> {
        let state = &self.cli_agent_session_restore;
        let Some(launch_cwd) = state
            .record
            .as_ref()
            .and_then(|record| record.launch_cwd.as_deref())
        else {
            return Some(true);
        };
        match &state.launch_directory {
            LaunchDirectoryCheck::Checked {
                launch_cwd: checked,
                exists,
            } if checked == launch_cwd => Some(*exists),
            LaunchDirectoryCheck::NotChecked
            | LaunchDirectoryCheck::Checking(_)
            | LaunchDirectoryCheck::Checked { .. } => None,
        }
    }

    /// Starts checking, off the main thread, whether the launch directory of a session the pane
    /// could resume exists here.
    fn check_cli_agent_launch_directory(&mut self, ctx: &mut ViewContext<Self>) {
        let state = &mut self.cli_agent_session_restore;
        let Some(launch_cwd) = state
            .record
            .as_ref()
            .filter(|record| record.can_resume_here())
            .and_then(|record| record.launch_cwd.clone())
        else {
            return;
        };
        let already_checked = match &state.launch_directory {
            LaunchDirectoryCheck::NotChecked => false,
            LaunchDirectoryCheck::Checking(checking) => *checking == launch_cwd,
            LaunchDirectoryCheck::Checked {
                launch_cwd: checked,
                ..
            } => *checked == launch_cwd,
        };
        if already_checked {
            return;
        }

        #[cfg(not(target_family = "wasm"))]
        {
            state.launch_directory = LaunchDirectoryCheck::Checking(launch_cwd.clone());
            let path = launch_cwd.clone();
            let _ = ctx.spawn(
                async move { std::path::Path::new(&path).is_dir() },
                move |me, exists, ctx| {
                    me.on_cli_agent_launch_directory_checked(launch_cwd, exists, ctx)
                },
            );
        }
        // There's no local file system to check.
        #[cfg(target_family = "wasm")]
        {
            let _ = ctx;
            state.launch_directory = LaunchDirectoryCheck::Checked {
                launch_cwd,
                exists: true,
            };
        }
    }

    fn on_cli_agent_launch_directory_checked(
        &mut self,
        launch_cwd: String,
        exists: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        if state.launch_directory != LaunchDirectoryCheck::Checking(launch_cwd.clone()) {
            // The record changed while the check ran.
            return;
        }
        state.launch_directory = LaunchDirectoryCheck::Checked { launch_cwd, exists };
        if exists {
            self.try_send_queued_cli_agent_resume(ctx);
            return;
        }
        log::info!("The launch directory of a restored CLI agent session doesn't exist here");
        if let Some(banner) = state.banner.clone() {
            banner.update(ctx, |banner, ctx| {
                banner.set_launch_directory_missing(true, ctx);
            });
        }
        match state.phase {
            // The sidebar may offer to resume it.
            RecoveryPhase::Interrupted => ctx.emit(Event::TerminalViewStateChanged),
            RecoveryPhase::ResumeQueued => {
                self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::NotAttempted, ctx);
            }
            // A `cd` that was already sent fails and offers the session again.
            RecoveryPhase::Tracking | RecoveryPhase::ResumePending => {}
        }
    }

    /// Fork: how the commands for the record bring its session back, or `None` until that's
    /// known: whether the conversation of a session saved before it did anything exists here is
    /// checked off the main thread (see [`Self::check_cli_agent_conversation`]).
    fn cli_agent_session_command(&self) -> Option<SessionCommand> {
        let state = &self.cli_agent_session_restore;
        let record = state.record.as_ref()?;
        if !record.awaiting_first_prompt {
            return Some(SessionCommand::Resume);
        }
        let Some(session_id) = record.session_id.as_deref() else {
            return Some(record.session_command(None));
        };
        match &state.conversation {
            ConversationCheck::Checked {
                session_id: checked,
                exists,
            } if checked == session_id => Some(record.session_command(*exists)),
            ConversationCheck::NotChecked
            | ConversationCheck::Checking(_)
            | ConversationCheck::Checked { .. } => None,
        }
    }

    /// Fork: starts checking, off the main thread, whether the conversation of a record saved
    /// before its session did anything exists here.
    fn check_cli_agent_conversation(&mut self, ctx: &mut ViewContext<Self>) {
        let state = &mut self.cli_agent_session_restore;
        let Some((session_id, record)) = state
            .record
            .as_ref()
            .filter(|record| record.awaiting_first_prompt)
            .and_then(|record| Some((record.session_id.clone()?, record.clone())))
        else {
            return;
        };
        let already_checked = match &state.conversation {
            ConversationCheck::NotChecked => false,
            ConversationCheck::Checking(checking) => *checking == session_id,
            ConversationCheck::Checked {
                session_id: checked,
                ..
            } => *checked == session_id,
        };
        if already_checked {
            return;
        }

        #[cfg(not(target_family = "wasm"))]
        {
            use crate::terminal::cli_agent_sessions::restore::transcript_exists;

            state.conversation = ConversationCheck::Checking(session_id.clone());
            let _ = ctx.spawn(
                async move { transcript_exists(&record) },
                move |me, exists, ctx| {
                    me.on_cli_agent_conversation_checked(session_id, exists, ctx)
                },
            );
        }
        // There's no local file system to check.
        #[cfg(target_family = "wasm")]
        {
            let _ = (ctx, record);
            state.conversation = ConversationCheck::Checked {
                session_id,
                exists: None,
            };
        }
    }

    fn on_cli_agent_conversation_checked(
        &mut self,
        session_id: String,
        exists: Option<bool>,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        if state.conversation != ConversationCheck::Checking(session_id.clone()) {
            // The record changed while the check ran.
            return;
        }
        state.conversation = ConversationCheck::Checked { session_id, exists };
        let starts_new_session = self.cli_agent_session_command() == Some(SessionCommand::StartNew);
        if let Some(banner) = self.cli_agent_session_restore.banner.clone() {
            banner.update(ctx, |banner, ctx| {
                banner.set_starts_new_session(starts_new_session, ctx);
            });
        }
        self.try_send_queued_cli_agent_resume(ctx);
    }

    /// Acts on the login shell having bootstrapped: shows a deferred banner and runs a queued
    /// resume. Which bootstrap event arrives first varies, so this is called from several; only
    /// the first call acts.
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
        if std::mem::take(&mut state.show_banner_after_bootstrap) {
            let previous_resume = state.previous_resume;
            self.show_cli_agent_session_restore_banner(previous_resume, ctx);
        }
        if self.cli_agent_session_restore.phase == RecoveryPhase::ResumeQueued {
            self.arm_cli_agent_resume_timer(RESUME_QUEUE_TIMEOUT, ctx);
            self.schedule_queued_cli_agent_resume(ctx);
        }
    }

    /// Called when the shell reports its prompt without a completed block
    /// (`BootstrapPrecmdDone`): runs a queued resume right away, as pending commands are.
    pub(super) fn on_shell_prompt_for_cli_agent_restore(&mut self, ctx: &mut ViewContext<Self>) {
        self.on_login_shell_bootstrapped_for_cli_agent_restore(ctx);
        self.try_send_queued_cli_agent_resume(ctx);
    }

    /// Called after a block completes in a bootstrapped shell, including the bootstrap blocks
    /// that precede the first prompt.
    pub(super) fn on_block_completed_for_cli_agent_restore(&mut self, ctx: &mut ViewContext<Self>) {
        let was_ready = self.cli_agent_session_restore.shell_ready_handled;
        self.on_login_shell_bootstrapped_for_cli_agent_restore(ctx);
        if was_ready && self.cli_agent_session_restore.phase == RecoveryPhase::ResumeQueued {
            self.schedule_queued_cli_agent_resume(ctx);
        }
    }

    /// Puts the banner back after the block list was cleared (e.g. Cmd-K), which drops all rich
    /// content.
    pub(super) fn on_block_list_cleared_for_cli_agent_restore(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.cli_agent_session_restore.banner.take().is_none() {
            return;
        }
        let previous_resume = self.cli_agent_session_restore.previous_resume;
        self.show_cli_agent_session_restore_banner(previous_resume, ctx);
    }

    /// What the pane's running command says about the `agent` session reporting through it:
    /// whether typing the resume command into a fresh local shell resumes it (the pane runs a
    /// local login shell, no subshell, and the command doesn't go through a remote or container
    /// wrapper such as `ssh` or `tmux`), and whether the command is the agent itself, as opposed
    /// to e.g. a script that runs `claude -p`.
    fn observe_cli_agent_launch(&self, agent: CLIAgent, ctx: &AppContext) -> LaunchObservation {
        let Some(session_id) = self.active_block_session_id() else {
            return LaunchObservation::default();
        };
        let Some(session) = self.sessions.as_ref(ctx).get(session_id) else {
            return LaunchObservation::default();
        };
        let command = {
            let model = self.model.lock();
            let active_block = model.block_list().active_block();
            active_block
                .is_active_and_long_running()
                .then(|| active_block.command_with_secrets_obfuscated(false))
        };
        let Some(command) = command else {
            return LaunchObservation::default();
        };
        let escape_char = session.shell_family().escape_char();
        let started_as_agent =
            CLIAgent::detect(&command, Some(escape_char), Some(session.aliases()), ctx)
                == Some(agent)
                || (ctx.has_singleton_model::<CompiledCommandsForCodingAgentToolbar>()
                    && CompiledCommandsForCodingAgentToolbar::matched_agent(ctx, &command)
                        == Some(agent));
        let launched_locally = self.session_is_local(session_id, ctx)
            && session.subshell_info().is_none()
            && !runs_through_remote_wrapper(&command, escape_char, session.aliases());
        LaunchObservation {
            launched_locally,
            started_as_agent,
        }
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
            self.schedule_cli_agent_offer_check(event.terminal_view_id(), ctx);
            return;
        }

        let before = self.cli_agent_session_for_snapshot();
        let pane_cwd = self.pwd();
        let now = now_unix_ms();
        // Read the model rather than the event: events are delivered after the fact, and the
        // model has the latest state (e.g. the replacement after a session was replaced).
        let launch = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .map(|session| session.agent)
            .map(|agent| self.observe_cli_agent_launch(agent, ctx))
            .unwrap_or_default();
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
                    // Fork: a session that hasn't done anything yet is saved too, so a restart
                    // brings it back, but it doesn't replace a record of one that has.
                    let keep_awaiting_first_prompt = state.record.as_ref().is_none_or(|record| {
                        state.record_is_live_instance && record.awaiting_first_prompt
                    });
                    let live = if keep_awaiting_first_prompt {
                        PersistedCLIAgentSession::from_live_session_or_awaiting_first_prompt(
                            session,
                            pane_cwd.as_deref(),
                            previous,
                            launch,
                            now,
                        )
                    } else {
                        PersistedCLIAgentSession::from_live_session(
                            session,
                            pane_cwd.as_deref(),
                            previous,
                            launch,
                            now,
                        )
                    };
                    if let Some(live) = live {
                        state.record = Some(live);
                        state.record_is_live_instance = true;
                    }
                }
                RecoveryPhase::Interrupted | RecoveryPhase::ResumeQueued => {
                    // A new interactive agent session that has done something replaces the
                    // interrupted one. A headless run (e.g. `claude -p` in a script) doesn't.
                    if let Some(live) = PersistedCLIAgentSession::from_live_session(
                        session,
                        pane_cwd.as_deref(),
                        None,
                        launch,
                        now,
                    )
                    .filter(|live| live.evidently_interactive)
                    {
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
                    let live = if state.resume_starts_new_session {
                        // Fork: the new session shares nothing with the restored one but its
                        // directory, and replaces it as soon as it reports its id.
                        PersistedCLIAgentSession::from_live_session_or_awaiting_first_prompt(
                            session,
                            pane_cwd.as_deref(),
                            None,
                            launch,
                            now,
                        )
                    } else {
                        PersistedCLIAgentSession::from_live_session(
                            session,
                            pane_cwd.as_deref(),
                            restored,
                            launch,
                            now,
                        )
                    };
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
                        state.resume_acknowledged = true;
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
        // A status change leaves the record as it is, but the saved app state also keeps when
        // the session last changed status (see `cli_agent_session_for_app_state`). That can wait
        // for a lazy save.
        if matches!(event, CLIAgentSessionsModelEvent::StatusChanged { .. })
            && self.cli_agent_session_restore.record.is_some()
            && ctx.has_singleton_model::<CLIAgentRestoreSaveScheduler>()
        {
            CLIAgentRestoreSaveScheduler::handle(ctx).update(ctx, |scheduler, ctx| {
                scheduler.request_lazy_save(ctx);
            });
        }
        self.refresh_cli_agent_session_name(ctx);
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
                    self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx);
                }
                return;
            }
            Some(ResumeStep::Agent) => {
                state.record_is_live_instance = false;
                if state.record.is_none() {
                    return;
                }
                let acknowledged = std::mem::take(&mut state.resume_acknowledged);
                match (succeeded, acknowledged) {
                    // The resumed agent was quit deliberately.
                    (true, true) => self.clear_cli_agent_session_record(ctx),
                    // Without the agent reporting the session, it may not have resumed at all
                    // (e.g. it didn't find the session), so the session stays on offer.
                    (true, false) => self.mark_cli_agent_session_interrupted(
                        PreviousResumeAttempt::Unconfirmed,
                        ctx,
                    ),
                    (false, true) => self.mark_cli_agent_session_interrupted(
                        PreviousResumeAttempt::NotAttempted,
                        ctx,
                    ),
                    (false, false) => {
                        self.mark_cli_agent_session_interrupted(PreviousResumeAttempt::Failed, ctx)
                    }
                }
                return;
            }
            None => {}
        }

        if !agent_block_was_active {
            return;
        }
        state.record_is_live_instance = false;
        let previous_resume = match state.phase {
            RecoveryPhase::Tracking => PreviousResumeAttempt::NotAttempted,
            RecoveryPhase::ResumePending => PreviousResumeAttempt::Failed,
            RecoveryPhase::Interrupted | RecoveryPhase::ResumeQueued => return,
        };
        let Some(record) = state.record.as_ref() else {
            return;
        };
        // A headless run that failed (e.g. `claude -p` in a failing script) isn't worth
        // offering.
        let headless_run_failed =
            state.phase == RecoveryPhase::Tracking && !succeeded && !record.evidently_interactive;
        // Fork: neither is a session that exited without doing anything; it was only kept for
        // a restart.
        let never_used = state.phase == RecoveryPhase::Tracking && record.awaiting_first_prompt;
        if succeeded || headless_run_failed || never_used {
            self.clear_cli_agent_session_record(ctx);
        } else {
            self.mark_cli_agent_session_interrupted(previous_resume, ctx);
        }
    }

    fn mark_cli_agent_session_interrupted(
        &mut self,
        previous_resume: PreviousResumeAttempt,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        state.cancel_resume();
        state.resume_acknowledged = false;
        if state.record.is_none() {
            self.set_cli_agent_recovery_phase(RecoveryPhase::Tracking, ctx);
            return;
        }
        state.record_is_live_instance = false;
        self.set_cli_agent_recovery_phase(RecoveryPhase::Interrupted, ctx);
        self.check_cli_agent_launch_directory(ctx);
        self.check_cli_agent_conversation(ctx);
        self.show_cli_agent_session_restore_banner(previous_resume, ctx);
        ctx.notify();
    }

    fn clear_cli_agent_session_record(&mut self, ctx: &mut ViewContext<Self>) {
        let before = self.cli_agent_session_for_snapshot();
        let state = &mut self.cli_agent_session_restore;
        state.record = None;
        state.record_is_live_instance = false;
        state.show_banner_after_bootstrap = false;
        state.previous_resume = PreviousResumeAttempt::NotAttempted;
        state.launch_directory = LaunchDirectoryCheck::NotChecked;
        state.conversation = ConversationCheck::NotChecked;
        state.resume_starts_new_session = false;
        state.in_flight_resume_step = None;
        state.resume_acknowledged = false;
        state.bootstrap_fallback_deadline = None;
        state.cancel_resume();
        self.set_cli_agent_recovery_phase(RecoveryPhase::Tracking, ctx);
        self.remove_cli_agent_session_restore_banner(ctx);
        ctx.notify();
        self.save_if_cli_agent_record_changed(before, ctx);
    }

    /// Stops offering the interrupted session and forgets it.
    pub(crate) fn dismiss_restored_cli_agent_session(&mut self, ctx: &mut ViewContext<Self>) {
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
        self.sync_cli_agent_resume_claim(ctx);
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

    /// Copies the command that resumes the session this pane keeps to the clipboard.
    pub(crate) fn copy_restored_cli_agent_session_command(&mut self, ctx: &mut ViewContext<Self>) {
        let command = self
            .cli_agent_session_command()
            .unwrap_or(SessionCommand::Resume);
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
        let Some(command_line) = record.command_line(command, shell_type) else {
            return;
        };
        ctx.clipboard()
            .write(ClipboardContent::plain_text(command_line));
        let message = match command {
            SessionCommand::Resume => "Resume command copied",
            SessionCommand::StartNew => "Start command copied",
        };
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            let toast = DismissibleToast::default(message.to_owned());
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        });
    }

    fn show_cli_agent_session_restore_banner(
        &mut self,
        previous_resume: PreviousResumeAttempt,
        ctx: &mut ViewContext<Self>,
    ) {
        let state = &mut self.cli_agent_session_restore;
        if state.phase != RecoveryPhase::Interrupted {
            return;
        }
        state.previous_resume = previous_resume;
        if !state.shell_ready_handled {
            state.show_banner_after_bootstrap = true;
            return;
        }
        state.show_banner_after_bootstrap = false;
        let Some(record) = state.record.clone() else {
            return;
        };
        let launch_directory_missing = self.cli_agent_launch_directory_exists() == Some(false);
        let starts_new_session = self.cli_agent_session_command() == Some(SessionCommand::StartNew);
        self.remove_cli_agent_session_restore_banner(ctx);

        let banner_record = record.clone();
        let banner = ctx.add_typed_action_view(move |_| {
            CLIAgentSessionRestoreBanner::new(
                banner_record,
                previous_resume,
                launch_directory_missing,
                starts_new_session,
            )
        });
        ctx.subscribe_to_view(&banner, |me, _, event, ctx| match event {
            CLIAgentSessionRestoreBannerEvent::Resume => {
                me.resume_restored_cli_agent_session_with_feedback(ctx);
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

        // Fork: for a session saved before it did anything, a missing transcript means it's
        // started anew (see `check_cli_agent_conversation`), not that resuming may fail.
        self.check_cli_agent_conversation(ctx);
        #[cfg(not(target_family = "wasm"))]
        if record.can_resume_here() && !record.awaiting_first_prompt {
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
