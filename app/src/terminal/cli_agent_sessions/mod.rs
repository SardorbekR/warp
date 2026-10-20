pub mod event;
pub(crate) mod launch_command;
pub mod listener;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod plugin_manager;
pub mod restore;
pub mod restore_save;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use event::{CLIAgentEvent, CLIAgentEventSource, CLIAgentEventType};
use warp_core::execution_mode::AppExecutionMode;
use warpui::r#async::SpawnedFutureHandle;
use warpui::{
    AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity, WeakViewHandle,
};

use self::listener::CLIAgentSessionListener;
use super::{CLIAgent, TerminalView};
use crate::ai::blocklist::InputConfig;

/// How long to wait, after observing a synthesized Ctrl-C write to a working
/// CLI agent session's PTY, for further plugin activity before concluding the
/// interrupt silently cancelled the session. See `observe_ctrl_c_write`.
pub const CTRL_C_CANCEL_WINDOW: Duration = Duration::from_secs(2);

const NEEDS_INPUT_STATUS_MESSAGE: &str = "Agent requires user input";

/// How long to wait, after a lone Escape byte is written to the PTY of an agent in the middle of
/// a turn, for further plugin activity before concluding it interrupted the turn. Claude Code
/// reports no `Stop` when Escape interrupts it. See `observe_escape_write`.
pub const ESCAPE_TURN_END_WINDOW: Duration = CTRL_C_CANCEL_WINDOW;

/// How many nested session ids (see [`CLIAgentSessionContext::nested_session_ids`]) are kept.
const MAX_NESTED_SESSION_IDS: usize = 16;

/// What caused a session to become [`CLIAgentSessionStatus::Blocked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedSource {
    PermissionRequest,
    QuestionAsked,
    NeedsInput,
}

/// Status of a tracked CLI agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CLIAgentSessionStatus {
    InProgress,
    Success,
    Failed {
        error_type: Option<String>,
        message: Option<String>,
    },
    Blocked {
        message: Option<String>,
        source: BlockedSource,
    },
    /// The user interrupted the session with Ctrl-C and no further plugin
    /// activity was observed within the grace window (see
    /// `observe_ctrl_c_write`). Not terminal: a later `prompt_submit`
    /// returns the session to `InProgress` like any other resumed turn.
    Cancelled,
}

impl CLIAgentSessionStatus {
    pub fn to_conversation_status(&self) -> crate::ai::agent::conversation::ConversationStatus {
        use crate::ai::agent::conversation::ConversationStatus;
        match self {
            CLIAgentSessionStatus::InProgress => ConversationStatus::InProgress,
            CLIAgentSessionStatus::Success => ConversationStatus::Success,
            CLIAgentSessionStatus::Failed { .. } => ConversationStatus::Error,
            CLIAgentSessionStatus::Blocked { message, .. } => ConversationStatus::Blocked {
                blocked_action: message.clone().unwrap_or_default(),
            },
            CLIAgentSessionStatus::Cancelled => ConversationStatus::Cancelled,
        }
    }
}

/// Rich context accumulated from CLI agent session events.
#[derive(Debug, Clone, Default)]
pub struct CLIAgentSessionContext {
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub summary: Option<String>,
    pub query: Option<String>,
    pub response: Option<String>,
    /// The session id this agent instance would be resumed with. It follows the agent switching
    /// sessions (`/clear`, which the plugin reports only through the new id's own events, or an
    /// in-session `/resume`, reported by a `SessionStart`), but not nested runs of the agent
    /// (e.g. `claude -p` started by its shell tool) writing to the same terminal: their
    /// `SessionStart` arrives during a turn, and their ids are kept in `nested_session_ids`.
    /// Unlike `session_id`, it ignores other agents' events.
    pub resume_session_id: Option<String>,
    /// The first `cwd` reported for `resume_session_id`. Unlike `cwd`, it doesn't follow the
    /// agent around, so it identifies where the session can be resumed from.
    pub launch_cwd: Option<String>,
    /// Transcript file for `resume_session_id`, when the plugin has reported it.
    pub transcript_path: Option<String>,
    /// Whether the `resume_session_id` session is in the middle of a turn: a prompt was submitted
    /// and the agent hasn't stopped since.
    pub resume_turn_in_progress: bool,
    /// Whether `query` was last written by an event from another session than
    /// `resume_session_id` (a nested run or another agent), so it doesn't describe that session.
    pub query_from_other_session: bool,
    /// Ids announced by a `SessionStart` during a turn: nested runs of the agent, whose events
    /// don't switch `resume_session_id` unless they show a prompt only an interactive session
    /// shows. Oldest first, at most [`MAX_NESTED_SESSION_IDS`].
    pub nested_session_ids: VecDeque<String>,
    /// Whether the agent showed a prompt only an interactive session shows (an idle prompt or a
    /// permission request), unlike e.g. `claude -p` run by a script.
    pub showed_interactive_prompt: bool,
}

/// State of the rich input editor for composing a prompt to send to a CLI agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CLIAgentInputState {
    /// The rich input editor is not open.
    Closed,
    /// The rich input editor is open.
    Open {
        /// How this session was opened (for telemetry).
        entrypoint: CLIAgentInputEntrypoint,
        /// The input config that was active before opening rich input.
        previous_input_config: InputConfig,
        /// Whether the previous lock state was established while the input buffer was empty.
        previous_was_lock_set_with_empty_buffer: bool,
    },
}

/// Why the CLI agent rich input was closed (for telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CLIAgentRichInputCloseReason {
    /// User explicitly closed (Escape, Ctrl-G, footer button).
    Manual,
    /// Auto-closed due to agent status change (e.g. Blocked).
    AutoToggle,
    /// Auto-dismissed after submitting a prompt.
    Submit,
    /// Closed for another reason (chip removed, session ended, shared session sync).
    Other,
}

/// How a [`CLIAgentInputState`] was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CLIAgentInputEntrypoint {
    /// User pressed Ctrl-G while a CLI agent was active.
    CtrlG,
    /// User clicked the rich input button in the CLI agent footer.
    FooterButton,
    /// Automatically opened when the CLI agent resumed work (left a blocked state)
    /// and the auto-show setting is enabled.
    AutoShow,
    /// Rich input was opened to mirror a shared-session participant's state.
    SharedSessionSync,
}

impl CLIAgentSessionContext {
    /// The fields that identify which agent session this is and where it can be resumed from.
    fn identity(&self) -> (Option<String>, Option<String>, Option<String>) {
        (
            self.resume_session_id.clone(),
            self.launch_cwd.clone(),
            self.transcript_path.clone(),
        )
    }

    pub(crate) fn display_title(&self) -> Option<String> {
        self.latest_user_prompt().or_else(|| self.title_like_text())
    }

    pub(crate) fn latest_user_prompt(&self) -> Option<String> {
        self.query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_owned)
    }

    /// Returns summary text suitable as a fallback title when no user prompt is available.
    pub(crate) fn title_like_text(&self) -> Option<String> {
        self.summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .map(str::to_owned)
    }

    /// Whether an event of `event_type` from the same agent that reports `session_id` switches
    /// the resume identity to that session. A `SessionStart` with a new id during a turn comes
    /// from a nested run, whose id is remembered so its later events are ignored too. Any other
    /// event with an id that wasn't announced that way comes from the agent itself having
    /// switched sessions (`/clear` reports no `SessionStart`). A nested id is adopted once it
    /// shows a prompt that headless runs (`claude -p`) never show.
    fn adopts_reported_session(
        &mut self,
        session_id: &str,
        event_type: &CLIAgentEventType,
    ) -> bool {
        match self.resume_session_id.as_deref() {
            None => return true,
            Some(current) if current == session_id => return false,
            Some(_) => {}
        }
        let nested = self.nested_session_ids.iter().any(|id| id == session_id);
        let adopts = match event_type {
            CLIAgentEventType::SessionStart => {
                if self.resume_turn_in_progress && !nested {
                    self.nested_session_ids.push_back(session_id.to_owned());
                    while self.nested_session_ids.len() > MAX_NESTED_SESSION_IDS {
                        self.nested_session_ids.pop_front();
                    }
                }
                !self.resume_turn_in_progress
            }
            CLIAgentEventType::IdlePrompt | CLIAgentEventType::PermissionRequest => true,
            CLIAgentEventType::PromptSubmit
            | CLIAgentEventType::ToolComplete
            | CLIAgentEventType::Stop
            | CLIAgentEventType::StopFailure
            | CLIAgentEventType::PermissionReplied
            | CLIAgentEventType::QuestionAsked
            | CLIAgentEventType::NeedsInput
            | CLIAgentEventType::Unknown(_) => !nested,
        };
        if adopts {
            self.nested_session_ids.retain(|id| id != session_id);
        }
        adopts
    }

    /// Updates the resume identity from an event that reported `session_id` (after `cwd` was
    /// updated from it). `replaces_session` says whether the event may switch an agent instance
    /// that already has a session to `session_id` (see [`Self::adopts_reported_session`]).
    fn sync_resume_identity(
        &mut self,
        session_id: Option<&str>,
        replaces_session: bool,
        transcript_path: Option<&str>,
    ) {
        let previous_session_id = self.resume_session_id.clone();
        if let Some(session_id) = session_id
            && (previous_session_id.is_none() || replaces_session)
        {
            self.resume_session_id = Some(session_id.to_owned());
        }
        if self.resume_session_id != previous_session_id {
            self.launch_cwd = None;
            self.transcript_path = None;
            self.resume_turn_in_progress = false;
            // Prompts and tool state from the previous session don't describe the new one. The
            // first id reported for a session that was detected from its command is the same
            // session, so its state is kept.
            if previous_session_id.is_some() {
                self.query = None;
                self.query_from_other_session = false;
                self.response = None;
                self.summary = None;
                self.tool_name = None;
                self.tool_input_preview = None;
            }
        }
        // A directory or transcript reported along with another session's id describes that
        // session, not the one this instance resumes.
        if session_id
            .is_some_and(|session_id| self.resume_session_id.as_deref() != Some(session_id))
        {
            return;
        }
        if self.launch_cwd.is_none() {
            self.launch_cwd = self.cwd.clone();
        }
        if let Some(transcript_path) = transcript_path {
            self.transcript_path = Some(transcript_path.to_owned());
        }
    }
}

/// A tracked CLI agent session.
#[derive(Debug, Clone)]
pub struct CLIAgentSession {
    pub agent: CLIAgent,
    pub status: CLIAgentSessionStatus,
    pub session_context: CLIAgentSessionContext,
    /// Rich input editor state.
    pub input_state: CLIAgentInputState,
    /// Whether status-driven auto-toggle is enabled for this session.
    pub should_auto_toggle_input: bool,
    /// Event listener for plugin-backed sessions or Codex OSC9 fallback.
    /// `None` for non-Codex sessions created by command detection alone.
    /// Dropping this handle cleans up the listener's PTY event subscription.
    pub listener: Option<ModelHandle<CLIAgentSessionListener>>,
    /// The plugin version reported by structured plugin events.
    /// `None` if the plugin predates version reporting or Codex is using OSC9 fallback.
    pub plugin_version: Option<String>,
    /// `None` when the session is local.
    /// `Some("user@hostname")` when running over SSH (warpified or legacy).
    /// Used as a key for per-host plugin install failure tracking.
    pub remote_host: Option<String>,
    /// Draft text saved from the rich input composer when it was closed.
    /// Restored into the editor when the composer is reopened.
    pub draft_text: Option<String>,
    /// When the session was detected via a custom toolbar command pattern,
    /// the first word of the command (the binary/alias the user typed).
    /// Used to customize plugin instructions and force manual install mode.
    pub custom_command_prefix: Option<String>,
    /// Set once the session has received any structured OSC 777 (rich)
    /// notification. Codex's OSC 9 fallback never sets it, so this is the
    /// single source of truth for whether the session is plugin-backed.
    pub received_rich_notification: bool,
}

impl CLIAgentSession {
    pub fn is_remote(&self) -> bool {
        self.remote_host.is_some()
    }

    /// Whether the session surfaces trustworthy fine-grained status
    /// (in-progress / blocked / success). True only after receiving a rich OSC
    /// 777 notification. Codex's OSC 9 fallback emits only opaque `Stop`
    /// notifications and never sets `received_rich_notification`, so it does
    /// not qualify. Synthetic listener registration also does not qualify until
    /// an actual rich notification arrives.
    pub fn supports_rich_status(&self) -> bool {
        self.received_rich_notification
    }

    /// Whether the session is blocked on a `NeedsInput` notification, so nothing should be
    /// typed into its terminal.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub fn is_blocked_on_needs_input(&self) -> bool {
        matches!(
            self.status,
            CLIAgentSessionStatus::Blocked {
                source: BlockedSource::NeedsInput,
                ..
            }
        )
    }

    /// Clears state populated by `PermissionRequest`. Called whenever the
    /// session leaves the permission flow (the user replied, a blocking tool
    /// completed, a new prompt is submitted, or the session ends successfully)
    /// so the permission summary doesn't leak into later UI surfaces — most
    /// visibly the tab title, which can fall back to `summary` when `query`
    /// is unset.
    fn clear_permission_scoped_state(&mut self) {
        self.session_context.summary = None;
        self.session_context.tool_name = None;
        self.session_context.tool_input_preview = None;
    }

    /// Updates the session that crash recovery would resume from `event`, and returns whether the
    /// event belongs to that session. Another agent's events (e.g. from a nested `codex exec`)
    /// never do, and events that don't name their agent never switch sessions. See
    /// [`CLIAgentSessionContext::adopts_reported_session`] for when the agent's own events do.
    fn sync_resume_identity_from_event(&mut self, event: &CLIAgentEvent) -> bool {
        let same_agent = event.agent == self.agent;
        if !same_agent && event.agent != CLIAgent::Unknown {
            return false;
        }
        let context = &mut self.session_context;
        let replaces_session = same_agent
            && event.session_id.as_deref().is_some_and(|session_id| {
                context.adopts_reported_session(session_id, &event.event)
            });
        context.sync_resume_identity(
            event.session_id.as_deref(),
            replaces_session,
            event.payload.transcript_path.as_deref(),
        );
        let from_resumed_session = event
            .session_id
            .as_deref()
            .is_none_or(|session_id| context.resume_session_id.as_deref() == Some(session_id));
        if from_resumed_session {
            match &event.event {
                // Tool and permission events only happen during a turn, so they also reopen one
                // that was taken for interrupted (see `observe_escape_write`).
                CLIAgentEventType::PromptSubmit
                | CLIAgentEventType::ToolComplete
                | CLIAgentEventType::PermissionRequest
                | CLIAgentEventType::PermissionReplied
                | CLIAgentEventType::QuestionAsked => context.resume_turn_in_progress = true,
                // With background work pending, the agent wakes itself again to finish the turn.
                CLIAgentEventType::Stop if event.payload.has_pending_background_work() => {}
                CLIAgentEventType::Stop
                | CLIAgentEventType::StopFailure
                | CLIAgentEventType::IdlePrompt => context.resume_turn_in_progress = false,
                CLIAgentEventType::SessionStart
                | CLIAgentEventType::NeedsInput
                | CLIAgentEventType::Unknown(_) => {}
            }
            if same_agent
                && matches!(
                    event.event,
                    CLIAgentEventType::IdlePrompt | CLIAgentEventType::PermissionRequest
                )
            {
                context.showed_interactive_prompt = true;
            }
        }
        from_resumed_session
    }

    /// Applies an event to this session, updating context and status.
    /// Returns the new status if it changed, or `None` if the event was irrelevant.
    fn apply_event(&mut self, event: &CLIAgentEvent) -> Option<CLIAgentSessionStatus> {
        self.session_context.cwd = event.cwd.clone().or(self.session_context.cwd.take());
        self.session_context.project = event
            .project
            .clone()
            .or(self.session_context.project.take());
        self.session_context.session_id = event
            .session_id
            .clone()
            .or(self.session_context.session_id.take());
        let from_resumed_session = self.sync_resume_identity_from_event(event);
        if matches!(
            event.event,
            CLIAgentEventType::PromptSubmit
                | CLIAgentEventType::Stop
                | CLIAgentEventType::StopFailure
        ) {
            self.session_context.query_from_other_session = !from_resumed_session;
        }

        let new_status = match &event.event {
            CLIAgentEventType::PromptSubmit => {
                self.session_context.query = event.payload.query.clone();
                self.session_context.response = None;
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::InProgress
            }
            CLIAgentEventType::ToolComplete => {
                if !matches!(self.status, CLIAgentSessionStatus::Blocked { .. }) {
                    return None;
                }
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::InProgress
            }
            CLIAgentEventType::Stop => {
                self.session_context.query = event.payload.query.clone();
                self.session_context.response = event.payload.response.clone();
                self.clear_permission_scoped_state();
                // The agent will wake itself again once that work finishes, so this turn
                // boundary is a pause rather than the end of the session.
                if event.payload.has_pending_background_work() {
                    return None;
                }
                CLIAgentSessionStatus::Success
            }
            CLIAgentEventType::StopFailure => {
                self.session_context.query = event.payload.query.clone();
                self.session_context.response = event.payload.response.clone();
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::Failed {
                    error_type: event.payload.error_type.clone(),
                    message: event.payload.response.clone(),
                }
            }
            CLIAgentEventType::PermissionRequest => {
                self.session_context.summary = event.payload.summary.clone();
                self.session_context.tool_name = event.payload.tool_name.clone();
                self.session_context.tool_input_preview = event.payload.tool_input_preview.clone();
                CLIAgentSessionStatus::Blocked {
                    message: event.payload.summary.clone(),
                    source: BlockedSource::PermissionRequest,
                }
            }
            CLIAgentEventType::QuestionAsked => CLIAgentSessionStatus::Blocked {
                message: event
                    .payload
                    .summary
                    .clone()
                    .or_else(|| Some("Waiting for your answer".to_owned())),
                source: BlockedSource::QuestionAsked,
            },
            CLIAgentEventType::NeedsInput => CLIAgentSessionStatus::Blocked {
                message: event
                    .payload
                    .summary
                    .clone()
                    .or_else(|| Some(NEEDS_INPUT_STATUS_MESSAGE.to_owned())),
                source: BlockedSource::NeedsInput,
            },
            CLIAgentEventType::PermissionReplied => {
                if !matches!(self.status, CLIAgentSessionStatus::Blocked { .. }) {
                    return None;
                }
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::InProgress
            }
            // IdlePrompt means the agent is sitting at its prompt waiting for input.
            // This should not affect status — otherwise it would override Success after a Stop event.
            CLIAgentEventType::IdlePrompt => return None,
            CLIAgentEventType::SessionStart => {
                // A repeated `SessionStart` (e.g. after `/clear`) that omits the version must not
                // erase the one reported when the session started.
                if let Some(plugin_version) = &event.payload.plugin_version {
                    self.plugin_version = Some(plugin_version.clone());
                }
                return None;
            }
            CLIAgentEventType::Unknown(_) => return None,
        };

        self.status = new_status.clone();
        Some(new_status)
    }
}

/// Events emitted by `CLIAgentSessionsModel` for subscribers (e.g., `AgentNotificationsModel`).
#[allow(dead_code)] // `agent` fields on Started/InputSessionChanged/Ended are used for logging and future subscribers.
#[derive(Debug, Clone)]
pub enum CLIAgentSessionsModelEvent {
    Started {
        terminal_view_id: EntityId,
        agent: CLIAgent,
    },
    StatusChanged {
        terminal_view_id: EntityId,
        agent: CLIAgent,
        status: CLIAgentSessionStatus,
        is_prompt_submit: bool,
        session_context: Box<CLIAgentSessionContext>,
    },
    InputSessionChanged {
        terminal_view_id: EntityId,
        agent: CLIAgent,
        /// The input state BEFORE this change. When transitioning from
        /// `Open` → `Closed`, contains the saved input config to restore.
        previous_input_state: CLIAgentInputState,
        /// The input state AFTER this change.
        new_input_state: CLIAgentInputState,
    },
    Ended {
        terminal_view_id: EntityId,
        agent: CLIAgent,
    },
    /// The agent session has been updated. Subscribers may use this as a trigger for best-effort
    /// saving of state derived from the agent's session.
    SessionUpdated {
        terminal_view_id: EntityId,
        agent: CLIAgent,
    },
}

impl CLIAgentSessionsModelEvent {
    pub fn terminal_view_id(&self) -> EntityId {
        match self {
            CLIAgentSessionsModelEvent::Started {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::InputSessionChanged {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::SessionUpdated {
                terminal_view_id, ..
            } => *terminal_view_id,
        }
    }
}

/// Per-session state for a Ctrl-C-initiated pending cancellation. Kept
/// separate from `CLIAgentSession` because it is synthesized entirely
/// client-side (see `observe_ctrl_c_write`) rather than reported by the
/// plugin protocol.
#[derive(Default)]
struct CtrlCCancelState {
    /// Whether a `prompt_submit` has been seen for this session. Guards
    /// against arming on the optimistic `InProgress` status set when a
    /// session is first registered, before any turn has actually started.
    has_seen_prompt_submit: bool,
    /// Abort handle for the in-flight grace-window timer, if armed.
    pending_cancel: Option<SpawnedFutureHandle>,
    /// Identifies the window `pending_cancel` belongs to. `SpawnedFutureHandle::abort`
    /// only takes effect the next time the future is polled, so a timer that has
    /// already completed (and queued its resolve callback) can still run after
    /// `abort()` is called. The callback captures this token and only acts if it
    /// still matches when it fires, so a stale callback racing a disarming event
    /// is a no-op instead of overwriting that event's status with `Cancelled`.
    armed_token: Option<u64>,
    /// Timer that ends the turn after a lone Escape went unanswered. See `observe_escape_write`.
    pending_turn_end: Option<SpawnedFutureHandle>,
    /// Identifies the window `pending_turn_end` belongs to, like `armed_token`.
    turn_end_token: Option<u64>,
}

/// What a pane does with the agent session it claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CLIAgentResumeClaimKind {
    /// The pane is resuming the session: the resume is queued or its commands were sent.
    Resuming,
    /// The pane tracks the session and persists it.
    Tracking,
}

/// An agent session a pane resumes or tracks, so that no other pane resumes it too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CLIAgentResumeClaim {
    pub agent: CLIAgent,
    pub session_id: String,
    pub kind: CLIAgentResumeClaimKind,
}

struct PaneResumeClaim {
    /// Claims of panes that no longer exist are ignored.
    pane: WeakViewHandle<TerminalView>,
    claim: CLIAgentResumeClaim,
}

impl PaneResumeClaim {
    /// Unlike upgrading `pane`, this also holds while the pane is being updated, e.g. when it
    /// runs another pane's update.
    fn pane_exists(&self, ctx: &AppContext) -> bool {
        self.pane.window_id(ctx).is_some()
    }
}

/// Singleton model that tracks pane-scoped CLI agent state and plugin-enriched session context.
pub struct CLIAgentSessionsModel {
    sessions: HashMap<EntityId, CLIAgentSession>,
    /// Tracks (agent, remote_host) pairs where an auto plugin operation (install or update) has failed.
    /// Shared across all views so failure in one tab is reflected everywhere.
    plugin_auto_failures: HashSet<(CLIAgent, Option<String>)>,
    /// Ctrl-C pending-cancel state, keyed by terminal view. See `observe_ctrl_c_write`.
    ctrl_c_cancel_state: HashMap<EntityId, CtrlCCancelState>,
    /// Source of `CtrlCCancelState::armed_token` values. Monotonically increasing;
    /// never reused, so a stale callback can never alias a newer window.
    next_ctrl_c_token: u64,
    /// The session each pane resumes or tracks, keyed by terminal view.
    resume_claims: HashMap<EntityId, PaneResumeClaim>,
}

impl Entity for CLIAgentSessionsModel {
    type Event = CLIAgentSessionsModelEvent;
}

impl SingletonEntity for CLIAgentSessionsModel {}

impl CLIAgentSessionsModel {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            plugin_auto_failures: HashSet::new(),
            ctrl_c_cancel_state: HashMap::new(),
            next_ctrl_c_token: 0,
            resume_claims: HashMap::new(),
        }
    }

    pub fn session(&self, terminal_view_id: EntityId) -> Option<&CLIAgentSession> {
        self.sessions.get(&terminal_view_id)
    }

    /// Whether `agent`'s session `session_id` is open in a pane other than `terminal_view_id`,
    /// so resuming it there would run it twice.
    pub fn is_session_open_elsewhere(
        &self,
        terminal_view_id: EntityId,
        agent: CLIAgent,
        session_id: &str,
    ) -> bool {
        self.sessions.iter().any(|(view_id, session)| {
            *view_id != terminal_view_id
                && session.agent == agent
                && session.session_context.resume_session_id.as_deref() == Some(session_id)
        })
    }

    /// Records the agent session `pane` resumes or tracks (`None` when it does neither), so other
    /// panes don't resume it too. Forgets the claims of panes that no longer exist.
    pub(crate) fn set_resume_claim(
        &mut self,
        pane: WeakViewHandle<TerminalView>,
        claim: Option<CLIAgentResumeClaim>,
        ctx: &mut ModelContext<Self>,
    ) {
        let pane_id = pane.id();
        self.resume_claims
            .retain(|view_id, existing| *view_id != pane_id && existing.pane_exists(ctx));
        if let Some(claim) = claim {
            self.resume_claims
                .insert(pane_id, PaneResumeClaim { pane, claim });
        }
    }

    /// How another existing pane than `terminal_view_id` claims `agent`'s session
    /// `session_id`, if one does.
    pub(crate) fn resume_claim_elsewhere(
        &self,
        terminal_view_id: EntityId,
        agent: CLIAgent,
        session_id: &str,
        ctx: &AppContext,
    ) -> Option<CLIAgentResumeClaimKind> {
        self.resume_claims
            .iter()
            .filter(|(view_id, pane_claim)| {
                **view_id != terminal_view_id
                    && pane_claim.claim.agent == agent
                    && pane_claim.claim.session_id == session_id
                    && pane_claim.pane_exists(ctx)
            })
            .map(|(_, pane_claim)| pane_claim.claim.kind)
            // A pane that tracks the session already runs it.
            .max_by_key(|kind| matches!(kind, CLIAgentResumeClaimKind::Tracking))
    }

    /// Returns `true` if the rich input editor is currently open for this terminal.
    pub fn is_input_open(&self, terminal_view_id: EntityId) -> bool {
        self.sessions
            .get(&terminal_view_id)
            .is_some_and(|s| matches!(s.input_state, CLIAgentInputState::Open { .. }))
    }

    /// Registers a plugin-backed listener on the session for this terminal.
    ///
    /// If a session for the same agent already exists (e.g. created earlier by
    /// command detection), it is upgraded with the listener and plugin context.
    /// Otherwise a new session is created.
    ///
    /// The optional `cwd` / `project` / `session_id` fields supply initial
    /// context when available (e.g. from a `SessionStart` event). Passing
    /// `None` for all three is fine — happens when the plugin is installed
    /// mid-session and there is no start event to extract context from.
    #[allow(clippy::too_many_arguments)]
    pub fn register_listener(
        &mut self,
        terminal_view_id: EntityId,
        agent: CLIAgent,
        cwd: Option<String>,
        project: Option<String>,
        session_id: Option<String>,
        plugin_version: Option<String>,
        remote_host: Option<String>,
        should_auto_toggle_input: bool,
        listener: ModelHandle<CLIAgentSessionListener>,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Some(session) = self
            .sessions
            .get_mut(&terminal_view_id)
            .filter(|s| s.agent == agent)
        {
            // Upgrade existing session with plugin context.
            session.status = CLIAgentSessionStatus::InProgress;
            session.listener = Some(listener);
            session.plugin_version = plugin_version;
            session.remote_host = remote_host;
            session.should_auto_toggle_input = should_auto_toggle_input;
            session.session_context.cwd = cwd.or(session.session_context.cwd.take());
            session.session_context.project = project.or(session.session_context.project.take());
            // The event that caused registration is applied next, so a `SessionStart` with
            // another id still switches the resume identity there.
            session
                .session_context
                .sync_resume_identity(session_id.as_deref(), false, None);
            session.session_context.session_id =
                session_id.or(session.session_context.session_id.take());
            return;
        }

        self.set_session(
            terminal_view_id,
            CLIAgentSession {
                agent,
                status: CLIAgentSessionStatus::InProgress,
                session_context: CLIAgentSessionContext {
                    resume_session_id: session_id.clone(),
                    launch_cwd: cwd.clone(),
                    cwd,
                    project,
                    session_id,
                    ..Default::default()
                },
                input_state: CLIAgentInputState::Closed,
                should_auto_toggle_input,
                listener: Some(listener),
                plugin_version,
                remote_host,
                draft_text: None,
                custom_command_prefix: None,
                received_rich_notification: false,
            },
            ctx,
        );
    }

    pub fn remove_session(&mut self, terminal_view_id: EntityId, ctx: &mut ModelContext<Self>) {
        self.abort_pending_cancel(terminal_view_id);
        self.abort_pending_turn_end(terminal_view_id);
        self.ctrl_c_cancel_state.remove(&terminal_view_id);
        if let Some(session) = self.sessions.remove(&terminal_view_id) {
            ctx.emit(CLIAgentSessionsModelEvent::Ended {
                terminal_view_id,
                agent: session.agent,
            });
        }
    }

    /// Updates the session's status and context from a parsed CLI agent event.
    /// Rich plugin events latch `received_rich_notification` so rich-status
    /// surfaces stay consistent even if the first event was not SessionStart.
    pub fn update_from_event(
        &mut self,
        terminal_view_id: EntityId,
        event: &CLIAgentEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.sessions.contains_key(&terminal_view_id) {
            return;
        }

        // Any plugin event other than `IdlePrompt` is evidence the CLI agent
        // process is still alive — an interrupt produces silence instead.
        // Disarm a pending Ctrl-C cancellation window so this event's own
        // status transition drives the session. `IdlePrompt` is excluded:
        // it means the CLI is sitting idle at its interactive prompt, which
        // is evidence of idleness rather than aliveness, so treating it as
        // disarming would let an idle notification that arrives instead of
        // a genuine `stop`/`stop_failure` after an interrupt silently defeat
        // the grace window, leaving the session stuck exactly like the bug
        // this feature exists to fix. `apply_event` still treats `IdlePrompt`
        // as a no-op for status, independent of this.
        if !matches!(event.event, CLIAgentEventType::IdlePrompt) {
            self.abort_pending_cancel(terminal_view_id);
        }
        // Any report from the agent settles the turn state itself.
        self.abort_pending_turn_end(terminal_view_id);
        if matches!(event.event, CLIAgentEventType::PromptSubmit) {
            self.ctrl_c_cancel_state
                .entry(terminal_view_id)
                .or_default()
                .has_seen_prompt_submit = true;
        }

        let session = self
            .sessions
            .get_mut(&terminal_view_id)
            .expect("session presence checked above");

        if event.source == CLIAgentEventSource::RichPlugin {
            session.received_rich_notification = true;
        }

        let event_type = &event.event;
        let identity_before = session.session_context.identity();
        // Interactive clients keep ignoring this event: only an unattended run has no one to
        // answer the prompt.
        let new_status = if matches!(event.event, CLIAgentEventType::NeedsInput)
            && !AppExecutionMode::as_ref(ctx).is_autonomous()
        {
            None
        } else {
            session.apply_event(event)
        };
        if let Some(new_status) = new_status {
            let agent = session.agent;
            ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                agent,
                status: new_status,
                is_prompt_submit: matches!(event_type, CLIAgentEventType::PromptSubmit),
                session_context: Box::new(session.session_context.clone()),
            });
        }

        // Identity changes (a new session id, launch directory or transcript) can arrive on any
        // event; subscribers that persist the session need to hear about them.
        let identity_changed = session.session_context.identity() != identity_before;
        if identity_changed
            || matches!(
                event_type,
                CLIAgentEventType::SessionStart
                    | CLIAgentEventType::PromptSubmit
                    | CLIAgentEventType::ToolComplete
            )
        {
            ctx.emit(CLIAgentSessionsModelEvent::SessionUpdated {
                terminal_view_id,
                agent: session.agent,
            });
        }
    }

    /// Observes a Ctrl-C byte (`0x03`) written to this session's PTY.
    ///
    /// This is observation only: the caller is responsible for forwarding the
    /// byte to the PTY unchanged and immediately, regardless of what this
    /// does. If the session is currently interruptible — `InProgress` or
    /// `Blocked`, rich-status-capable (excludes the Codex OSC 9 fallback),
    /// and has seen at least one `prompt_submit` (guarding against the
    /// optimistic `InProgress` set at registration, before any turn has
    /// started) — arms a grace window after which the session resolves to
    /// `Cancelled` if no disarming plugin activity arrives first (any event
    /// except `IdlePrompt`, which is evidence of idleness rather than
    /// aliveness; see `update_from_event`). A second Ctrl-C while a window
    /// is already armed reuses it rather than resetting the clock.
    pub fn observe_ctrl_c_write(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.observe_ctrl_c_write_with_window(terminal_view_id, CTRL_C_CANCEL_WINDOW, ctx);
    }

    fn observe_ctrl_c_write_with_window(
        &mut self,
        terminal_view_id: EntityId,
        window: Duration,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get(&terminal_view_id) else {
            return;
        };
        let can_arm = matches!(
            session.status,
            CLIAgentSessionStatus::InProgress | CLIAgentSessionStatus::Blocked { .. }
        ) && session.supports_rich_status()
            && self
                .ctrl_c_cancel_state
                .get(&terminal_view_id)
                .is_some_and(|state| state.has_seen_prompt_submit);
        if !can_arm {
            return;
        }
        if self
            .ctrl_c_cancel_state
            .get(&terminal_view_id)
            .is_some_and(|state| state.pending_cancel.is_some())
        {
            return;
        }

        let token = self.next_ctrl_c_token;
        self.next_ctrl_c_token += 1;
        let state = self
            .ctrl_c_cancel_state
            .entry(terminal_view_id)
            .or_default();
        let handle = ctx.spawn_abortable(
            async move { warpui::r#async::Timer::after(window).await },
            move |model, _, ctx| model.resolve_pending_cancel(terminal_view_id, token, ctx),
            |_, _| {},
        );
        state.pending_cancel = Some(handle);
        state.armed_token = Some(token);
    }

    /// Called when a session's pending-cancel window lapses with no
    /// disarming plugin event. Transitions the session to `Cancelled` unless
    /// `token` no longer matches the currently armed window — meaning this
    /// callback was already queued (post `Timer::after` completion, pre-poll)
    /// when the window was disarmed, replaced, or removed, and
    /// `SpawnedFutureHandle::abort` did not take effect in time to stop it.
    fn resolve_pending_cancel(
        &mut self,
        terminal_view_id: EntityId,
        token: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        let owns_current_window = self
            .ctrl_c_cancel_state
            .get_mut(&terminal_view_id)
            .is_some_and(|state| {
                if state.armed_token != Some(token) {
                    return false;
                }
                state.pending_cancel = None;
                state.armed_token = None;
                true
            });
        if !owns_current_window {
            return;
        }
        self.force_cancel(terminal_view_id, ctx);
    }

    /// Observes a lone Escape byte (`0x1b`) written to this session's PTY. Like Ctrl-C, Escape
    /// interrupts a Claude Code turn without any hook firing, which would leave the turn open
    /// forever and make the session's own later `SessionStart` (an in-session `/resume`) look
    /// like a nested run. If the session is in the middle of a turn, arms a grace window after
    /// which the turn counts as over unless the agent reports anything first. Only the turn
    /// changes: Escape also dismisses menus, so the status is left alone.
    pub fn observe_escape_write(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.observe_escape_write_with_window(terminal_view_id, ESCAPE_TURN_END_WINDOW, ctx);
    }

    fn observe_escape_write_with_window(
        &mut self,
        terminal_view_id: EntityId,
        window: Duration,
        ctx: &mut ModelContext<Self>,
    ) {
        let in_turn = self.sessions.get(&terminal_view_id).is_some_and(|session| {
            session.supports_rich_status() && session.session_context.resume_turn_in_progress
        });
        if !in_turn
            || self
                .ctrl_c_cancel_state
                .get(&terminal_view_id)
                .is_some_and(|state| state.pending_turn_end.is_some())
        {
            return;
        }
        let token = self.next_ctrl_c_token;
        self.next_ctrl_c_token = self.next_ctrl_c_token.saturating_add(1);
        let handle = ctx.spawn_abortable(
            async move { warpui::r#async::Timer::after(window).await },
            move |model, _, _| model.resolve_pending_turn_end(terminal_view_id, token),
            |_, _| {},
        );
        let state = self
            .ctrl_c_cancel_state
            .entry(terminal_view_id)
            .or_default();
        state.pending_turn_end = Some(handle);
        state.turn_end_token = Some(token);
    }

    fn resolve_pending_turn_end(&mut self, terminal_view_id: EntityId, token: u64) {
        let owns_current_window = self
            .ctrl_c_cancel_state
            .get_mut(&terminal_view_id)
            .is_some_and(|state| {
                if state.turn_end_token != Some(token) {
                    return false;
                }
                state.pending_turn_end = None;
                state.turn_end_token = None;
                true
            });
        if !owns_current_window {
            return;
        }
        if let Some(session) = self.sessions.get_mut(&terminal_view_id) {
            session.session_context.resume_turn_in_progress = false;
        }
    }

    fn abort_pending_turn_end(&mut self, terminal_view_id: EntityId) {
        if let Some(state) = self.ctrl_c_cancel_state.get_mut(&terminal_view_id) {
            state.turn_end_token = None;
            if let Some(handle) = state.pending_turn_end.take() {
                handle.abort();
            }
        }
    }

    /// Aborts and clears any armed pending-cancel window for this terminal.
    /// Invalidates the token so a callback already queued when the abort
    /// fires too late to matter (see `resolve_pending_cancel`) is a no-op.
    fn abort_pending_cancel(&mut self, terminal_view_id: EntityId) {
        if let Some(state) = self.ctrl_c_cancel_state.get_mut(&terminal_view_id) {
            state.armed_token = None;
            if let Some(handle) = state.pending_cancel.take() {
                handle.abort();
            }
        }
    }

    /// Sets `status` to `Cancelled` and emits `StatusChanged`, unless the
    /// session has already moved past `InProgress`/`Blocked` or no longer
    /// exists.
    fn force_cancel(&mut self, terminal_view_id: EntityId, ctx: &mut ModelContext<Self>) {
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };
        if !matches!(
            session.status,
            CLIAgentSessionStatus::InProgress | CLIAgentSessionStatus::Blocked { .. }
        ) {
            return;
        }

        session.status = CLIAgentSessionStatus::Cancelled;
        // The interrupted turn is over, so `/clear` or an in-session `/resume` may follow.
        session.session_context.resume_turn_in_progress = false;
        let agent = session.agent;
        let session_context = Box::new(session.session_context.clone());
        ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id,
            agent,
            status: CLIAgentSessionStatus::Cancelled,
            is_prompt_submit: false,
            session_context,
        });
    }

    /// Whether Ctrl-C cancellation has already resolved (`Cancelled`) or is
    /// still pending (the grace window is armed) for this session. Only
    /// used by tests.
    #[cfg(test)]
    pub(crate) fn has_pending_or_resolved_ctrl_c_cancel(&self, terminal_view_id: EntityId) -> bool {
        if matches!(
            self.sessions.get(&terminal_view_id).map(|s| &s.status),
            Some(CLIAgentSessionStatus::Cancelled)
        ) {
            return true;
        }
        self.ctrl_c_cancel_state
            .get(&terminal_view_id)
            .is_some_and(|state| state.pending_cancel.is_some())
    }

    pub fn open_input(
        &mut self,
        terminal_view_id: EntityId,
        entrypoint: CLIAgentInputEntrypoint,
        previous_input_config: InputConfig,
        previous_was_lock_set_with_empty_buffer: bool,
        should_auto_toggle_input: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };

        let previous_input_state = session.input_state;
        session.input_state = CLIAgentInputState::Open {
            entrypoint,
            previous_input_config,
            previous_was_lock_set_with_empty_buffer,
        };
        session.should_auto_toggle_input = should_auto_toggle_input;

        ctx.emit(CLIAgentSessionsModelEvent::InputSessionChanged {
            terminal_view_id,
            agent: session.agent,
            previous_input_state,
            new_input_state: session.input_state,
        });
    }

    pub fn close_input(
        &mut self,
        terminal_view_id: EntityId,
        should_auto_toggle_input: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };
        if session.input_state == CLIAgentInputState::Closed {
            return;
        }

        let previous_input_state = session.input_state;
        session.input_state = CLIAgentInputState::Closed;
        session.should_auto_toggle_input = should_auto_toggle_input;
        ctx.emit(CLIAgentSessionsModelEvent::InputSessionChanged {
            terminal_view_id,
            agent: session.agent,
            previous_input_state,
            new_input_state: CLIAgentInputState::Closed,
        });
    }

    pub fn set_session(
        &mut self,
        terminal_view_id: EntityId,
        session: CLIAgentSession,
        ctx: &mut ModelContext<Self>,
    ) {
        let agent = session.agent;
        // Close any open rich input before replacing, so subscribers can
        // restore input config before the session ends.
        self.close_input(terminal_view_id, false, ctx);
        // A fresh session must re-observe `prompt_submit` before Ctrl-C can
        // arm, and any pending window belonged to the session being replaced.
        self.abort_pending_cancel(terminal_view_id);
        self.abort_pending_turn_end(terminal_view_id);
        self.ctrl_c_cancel_state.remove(&terminal_view_id);
        if let Some(old) = self.sessions.insert(terminal_view_id, session) {
            ctx.emit(CLIAgentSessionsModelEvent::Ended {
                terminal_view_id,
                agent: old.agent,
            });
        }

        ctx.emit(CLIAgentSessionsModelEvent::Started {
            terminal_view_id,
            agent,
        });
    }

    /// Records that an auto plugin operation (install or update) failed for the given agent/host.
    /// `remote_host` is `None` for local sessions, `Some("user@hostname")` for remote.
    #[cfg(not(target_family = "wasm"))]
    pub fn record_plugin_auto_failure(&mut self, agent: CLIAgent, remote_host: Option<String>) {
        self.plugin_auto_failures.insert((agent, remote_host));
    }

    /// Saves draft text from the rich input composer for the given terminal.
    /// Stores `None` for empty or whitespace-only text.
    pub fn set_draft(&mut self, terminal_view_id: EntityId, text: String) {
        if let Some(session) = self.sessions.get_mut(&terminal_view_id) {
            session.draft_text = if text.trim().is_empty() {
                None
            } else {
                Some(text)
            };
        }
    }

    /// Clears any saved draft text for the given terminal.
    pub fn clear_draft(&mut self, terminal_view_id: EntityId) {
        if let Some(session) = self.sessions.get_mut(&terminal_view_id) {
            session.draft_text = None;
        }
    }

    /// Returns and clears the draft text for the given terminal, if any.
    pub fn take_draft(&mut self, terminal_view_id: EntityId) -> Option<String> {
        self.sessions
            .get_mut(&terminal_view_id)
            .and_then(|s| s.draft_text.take())
    }

    /// Whether an auto plugin operation has previously failed for this agent on this host.
    pub fn has_plugin_auto_failed(&self, agent: CLIAgent, remote_host: &Option<String>) -> bool {
        self.plugin_auto_failures
            .contains(&(agent, remote_host.clone()))
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
