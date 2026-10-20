use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use warp_core::features::FeatureFlag;
use warpui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity, ViewHandle, WindowId};

use crate::BlocklistAIHistoryModel;
use crate::ai::active_agent_views_model::{ActiveAgentViewsEvent, ActiveAgentViewsModel};
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent_management::notifications::{
    NotificationCategory, NotificationId, NotificationItem, NotificationItems, NotificationOrigin,
    NotificationSourceAgent,
};
use crate::ai::artifacts::Artifact;
use crate::ai::blocklist::{BlocklistAIHistoryEvent, ConversationStatusUpdate, QueuedQueryModel};
use crate::terminal::cli_agent_sessions::{
    CLIAgentSessionStatus, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::terminal::{CLIAgent, TerminalView};
use crate::workspace::util::is_terminal_view_in_same_tab;
use crate::workspace::{Workspace, WorkspaceRegistry};

/// Singleton model responsible for triggering in-app notifications on blocking conversation
/// status updates and tracking/storing these notifications for the notifications mailbox.
/// Tracks and stores notifications for both warp agent conversations and other supported
/// cli agent sessions.
pub struct AgentNotificationsModel {
    notifications: NotificationItems,
    /// Artifacts accumulated during the current turn for each conversation.
    /// Drained into the notification when a terminal state fires, cleared on InProgress.
    pub(crate) pending_artifacts: HashMap<AIConversationId, Vec<Artifact>>,
    /// When each terminal view's agent session last changed status, in unix
    /// milliseconds. Feeds the age labels and ordering of the status-ordered
    /// vertical tabs panel. Wall-clock time, unlike `Instant` (which stops
    /// while a Mac sleeps), so ages stay right across sleep, and a restored
    /// session's time from before a restart can be seeded.
    session_activity: HashMap<EntityId, i64>,
    /// Terminal views the user explicitly marked as unread.
    manually_unread: HashSet<EntityId>,
    /// The terminal view focused in each window, whether or not the window
    /// is active.
    focused_terminal_by_window: HashMap<WindowId, EntityId>,
    /// The terminal view focused in each window when it was last active,
    /// i.e. the one the user last saw focused there. Manual unread state is
    /// cleared only when this changes to the marked view.
    seen_terminal_by_window: HashMap<WindowId, EntityId>,
    /// Terminal views whose current CLI agent session has had a prompt
    /// submitted, with the agent session id reported at that prompt. A
    /// session is registered as in progress before its first prompt, so
    /// this tells a session that is working apart from one that is waiting.
    prompted_cli_sessions: HashMap<EntityId, Option<String>>,
}

fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Entity for AgentNotificationsModel {
    type Event = AgentManagementEvent;
}

impl SingletonEntity for AgentNotificationsModel {}

impl AgentNotificationsModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, move |me, _, event, ctx| {
            me.handle_history_event(event, ctx);
        });

        let cli_sessions_model = CLIAgentSessionsModel::handle(ctx);
        ctx.subscribe_to_model(&cli_sessions_model, |me, _, event, ctx| {
            me.handle_cli_agent_session_event(event, ctx);
        });

        let active_views_model = ActiveAgentViewsModel::handle(ctx);
        ctx.subscribe_to_model(&active_views_model, |me, _, event, ctx| {
            me.handle_active_agent_views_changed(event, ctx);
        });

        Self {
            notifications: NotificationItems::default(),
            pending_artifacts: HashMap::new(),
            session_activity: HashMap::new(),
            manually_unread: HashSet::new(),
            focused_terminal_by_window: HashMap::new(),
            seen_terminal_by_window: HashMap::new(),
            prompted_cli_sessions: HashMap::new(),
        }
    }

    /// When the agent session in `terminal_view_id` last changed status, in
    /// unix milliseconds, if a change has been observed since launch or
    /// seeded from the saved app state.
    pub(crate) fn last_activity_unix_ms(&self, terminal_view_id: EntityId) -> Option<i64> {
        self.session_activity.get(&terminal_view_id).copied()
    }

    fn record_session_activity(&mut self, terminal_view_id: EntityId) {
        self.session_activity
            .insert(terminal_view_id, now_unix_ms());
    }

    /// Seeds when the agent session restored into `terminal_view_id` last
    /// changed status, as saved with the app state, so its age and position
    /// survive a restart. A change observed since launch wins.
    pub(crate) fn seed_session_activity(
        &mut self,
        terminal_view_id: EntityId,
        unix_ms: i64,
        ctx: &mut ModelContext<Self>,
    ) {
        if unix_ms <= 0 {
            return;
        }
        // Never in the future, so a skewed clock can't keep a row on top.
        let unix_ms = unix_ms.min(now_unix_ms());
        if let Entry::Vacant(entry) = self.session_activity.entry(terminal_view_id) {
            entry.insert(unix_ms);
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Whether the terminal view has unread activity: either an unread
    /// notification or an explicit "Mark as unread".
    pub(crate) fn is_terminal_view_unread(&self, terminal_view_id: EntityId) -> bool {
        self.manually_unread.contains(&terminal_view_id)
            || self
                .notifications
                .has_unread_for_terminal_view(terminal_view_id)
    }

    pub(crate) fn is_terminal_view_manually_unread(&self, terminal_view_id: EntityId) -> bool {
        self.manually_unread.contains(&terminal_view_id)
    }

    /// Marks the terminal view as unread until the user focuses it again.
    pub(crate) fn mark_terminal_view_unread(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.manually_unread.insert(terminal_view_id) {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Clears both the manual unread mark and any unread notifications for
    /// the terminal view.
    pub(crate) fn mark_terminal_view_read(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        let cleared_manual = self.manually_unread.remove(&terminal_view_id);
        let cleared_notifications = self
            .notifications
            .mark_all_terminal_view_items_as_read(terminal_view_id);
        if cleared_manual || cleared_notifications {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Drops every "Mark as unread" mark, e.g. when the sidebar stops
    /// ordering rows by status (the only place the marks are shown).
    pub(crate) fn clear_manual_unread_marks(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.manually_unread.is_empty() {
            self.manually_unread.clear();
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Records that `focused_terminal_view_id` is now focused in `window_id`.
    /// When the window is active, the user sees that focus: if it moved to a
    /// view marked as unread, the mark is cleared. Focus returning to the
    /// view the user already saw focused (e.g. after a context menu closes)
    /// keeps the mark. Focus that moves while the window is inactive is
    /// seen once the window is activated; see
    /// [`Self::handle_window_activated_for_read_state`].
    pub(crate) fn handle_terminal_focus_for_read_state(
        &mut self,
        window_id: WindowId,
        focused_terminal_view_id: Option<EntityId>,
        is_active_window: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        if !FeatureFlag::VerticalTabsStatusOrder.is_enabled() {
            return;
        }
        match focused_terminal_view_id {
            Some(id) => self.focused_terminal_by_window.insert(window_id, id),
            None => self.focused_terminal_by_window.remove(&window_id),
        };
        if is_active_window {
            self.see_focused_terminal(window_id, ctx);
        }
    }

    /// The window became active: the user now sees whatever is focused in it,
    /// including a view focused while it was inactive (e.g. by clicking a
    /// notification), which clears that view's manual unread mark.
    pub(crate) fn handle_window_activated_for_read_state(
        &mut self,
        window_id: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !FeatureFlag::VerticalTabsStatusOrder.is_enabled() {
            return;
        }
        self.see_focused_terminal(window_id, ctx);
    }

    fn see_focused_terminal(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        let focused = self.focused_terminal_by_window.get(&window_id).copied();
        let previously_seen = match focused {
            Some(id) => self.seen_terminal_by_window.insert(window_id, id),
            None => self.seen_terminal_by_window.remove(&window_id),
        };
        if let Some(focused) = focused
            && previously_seen != Some(focused)
            && self.manually_unread.remove(&focused)
        {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Forgets the read state and activity of terminal views that no longer
    /// exist, and the focus of windows that no longer exist. A closed pane
    /// that can still be restored (undo close) keeps its state until it's
    /// gone for good.
    pub(crate) fn prune_closed_terminal_views(&mut self, ctx: &mut ModelContext<Self>) {
        let live_windows: HashSet<WindowId> = ctx.window_ids().collect();
        let is_view_alive = |view_id: EntityId| {
            live_windows
                .iter()
                .any(|window_id| ctx.view_name(*window_id, view_id).is_some())
        };
        let is_window_alive = |window_id: WindowId| live_windows.contains(&window_id);
        if self.retain_read_state(is_view_alive, is_window_alive) {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Keeps only the entries of live views and windows. Returns whether a
    /// manual unread mark was dropped.
    fn retain_read_state(
        &mut self,
        is_view_alive: impl Fn(EntityId) -> bool,
        is_window_alive: impl Fn(WindowId) -> bool,
    ) -> bool {
        let unread_count = self.manually_unread.len();
        self.manually_unread
            .retain(|view_id| is_view_alive(*view_id));
        self.session_activity
            .retain(|view_id, _| is_view_alive(*view_id));
        self.prompted_cli_sessions
            .retain(|view_id, _| is_view_alive(*view_id));
        for focus_by_window in [
            &mut self.focused_terminal_by_window,
            &mut self.seen_terminal_by_window,
        ] {
            focus_by_window.retain(|window_id, view_id| {
                is_window_alive(*window_id) && is_view_alive(*view_id)
            });
        }
        self.manually_unread.len() != unread_count
    }

    /// Whether the CLI agent session in `terminal_view_id` hasn't had a
    /// prompt submitted yet. `current_session_id` is the session's resume
    /// session id: when it differs from the one seen at the last prompt
    /// (e.g. after `/clear`), the new session hasn't been prompted either.
    pub(crate) fn is_cli_session_awaiting_first_prompt(
        &self,
        terminal_view_id: EntityId,
        current_session_id: Option<&str>,
    ) -> bool {
        fn normalized(session_id: Option<&str>) -> Option<&str> {
            session_id.map(str::trim).filter(|id| !id.is_empty())
        }
        match self.prompted_cli_sessions.get(&terminal_view_id) {
            None => true,
            Some(prompted_session_id) => matches!(
                (
                    normalized(prompted_session_id.as_deref()),
                    normalized(current_session_id),
                ),
                (Some(prompted), Some(current)) if prompted != current
            ),
        }
    }

    pub(crate) fn notifications(&self) -> &NotificationItems {
        &self.notifications
    }

    pub(crate) fn mark_item_read(&mut self, id: NotificationId, ctx: &mut ModelContext<Self>) {
        if self.notifications.mark_item_read(id) {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    pub(crate) fn mark_all_items_read(&mut self, ctx: &mut ModelContext<Self>) {
        if self.notifications.mark_all_items_read() {
            ctx.emit(AgentManagementEvent::AllNotificationsMarkedRead);
        }
    }

    /// Marks all notifications from the given terminal view as read.
    pub(crate) fn mark_items_from_terminal_view_read(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !FeatureFlag::HOANotifications.is_enabled() {
            return;
        }
        if self
            .notifications
            .mark_all_terminal_view_items_as_read(terminal_view_id)
        {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    fn handle_active_agent_views_changed(
        &mut self,
        event: &ActiveAgentViewsEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        // A pane or window went away; forget the state of views that are gone.
        if matches!(
            event,
            ActiveAgentViewsEvent::ConversationClosed { .. } | ActiveAgentViewsEvent::WindowClosed
        ) {
            self.prune_closed_terminal_views(ctx);
        }
        if !FeatureFlag::HOANotifications.is_enabled() {
            return;
        }

        match event {
            ActiveAgentViewsEvent::ConversationClosed { conversation_id } => {
                // When a conversation is closed, clean up its notifications
                // (as there's no conversation to navigate to when you click said notifications).
                if self
                    .notifications
                    .remove_by_origin(NotificationOrigin::Conversation(*conversation_id))
                {
                    ctx.emit(AgentManagementEvent::NotificationUpdated);
                }
            }
            ActiveAgentViewsEvent::TerminalViewFocused
            | ActiveAgentViewsEvent::WindowClosed
            | ActiveAgentViewsEvent::AmbientSessionOpened { .. }
            | ActiveAgentViewsEvent::AmbientSessionClosed { .. } => {}
        }
    }

    fn handle_cli_agent_session_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        // Track session activity independently of notifications so the
        // vertical tabs panel can order and age rows.
        match event {
            CLIAgentSessionsModelEvent::Started {
                terminal_view_id, ..
            } => {
                // A new session starts out waiting for its first prompt.
                self.prompted_cli_sessions.remove(terminal_view_id);
                self.record_session_activity(*terminal_view_id);
            }
            CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                is_prompt_submit,
                session_context,
                ..
            } => {
                if *is_prompt_submit {
                    // Keyed by the id the session would be resumed with, so a
                    // nested headless run (e.g. `claude -p` inside the agent)
                    // reporting its own id doesn't look like a new session.
                    self.prompted_cli_sessions
                        .insert(*terminal_view_id, session_context.resume_session_id.clone());
                }
                self.record_session_activity(*terminal_view_id);
            }
            CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            } => {
                self.session_activity.remove(terminal_view_id);
                self.prompted_cli_sessions.remove(terminal_view_id);
                // Sessions end when their pane closes, so also forget views
                // that are gone.
                self.prune_closed_terminal_views(ctx);
            }
            CLIAgentSessionsModelEvent::InputSessionChanged { .. }
            | CLIAgentSessionsModelEvent::SessionUpdated { .. } => {}
        }

        if !FeatureFlag::HOANotifications.is_enabled() {
            return;
        }

        match event {
            CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            } => {
                self.remove_notification_by_source(
                    NotificationOrigin::CLISession(*terminal_view_id),
                    ctx,
                );
            }
            CLIAgentSessionsModelEvent::Started { .. }
            | CLIAgentSessionsModelEvent::InputSessionChanged { .. }
            | CLIAgentSessionsModelEvent::SessionUpdated { .. } => {}
            CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                agent,
                status,
                session_context,
                ..
            } => match status {
                // When the agent resumes its work we can assume that the previous notification is stale.
                CLIAgentSessionStatus::InProgress => {
                    self.remove_notification_by_source(
                        NotificationOrigin::CLISession(*terminal_view_id),
                        ctx,
                    );
                }
                CLIAgentSessionStatus::Success => {
                    let title = session_context
                        .display_title()
                        .unwrap_or_else(|| format!("{} completed", agent.display_name()));
                    let message = match agent {
                        CLIAgent::Codex => "Notification from Codex",
                        _ => "Task completed.",
                    };
                    let metadata = TerminalViewMetadata::lookup(*terminal_view_id, ctx);
                    self.add_notification(
                        title,
                        message.to_owned(),
                        NotificationCategory::Complete,
                        NotificationSourceAgent::CLI {
                            agent: *agent,
                            is_ambient: metadata.is_ambient,
                        },
                        NotificationOrigin::CLISession(*terminal_view_id),
                        *terminal_view_id,
                        vec![],
                        metadata.branch,
                        ctx,
                    );
                }
                CLIAgentSessionStatus::Failed {
                    error_type,
                    message,
                } => {
                    let title = session_context
                        .display_title()
                        .unwrap_or_else(|| format!("{} failed", agent.display_name()));
                    let body = match (message.as_deref(), error_type.as_deref()) {
                        (Some(msg), Some(kind)) => format!("{kind}: {msg}"),
                        (Some(msg), None) => msg.to_owned(),
                        (None, Some(kind)) => kind.to_owned(),
                        (None, None) => "The agent encountered an error.".to_owned(),
                    };
                    let metadata = TerminalViewMetadata::lookup(*terminal_view_id, ctx);
                    self.add_notification(
                        title,
                        body,
                        NotificationCategory::Error,
                        NotificationSourceAgent::CLI {
                            agent: *agent,
                            is_ambient: metadata.is_ambient,
                        },
                        NotificationOrigin::CLISession(*terminal_view_id),
                        *terminal_view_id,
                        vec![],
                        metadata.branch,
                        ctx,
                    );
                }
                CLIAgentSessionStatus::Blocked { message, .. } => {
                    let title = session_context
                        .display_title()
                        .unwrap_or_else(|| format!("{} needs attention", agent.display_name()));
                    let metadata = TerminalViewMetadata::lookup(*terminal_view_id, ctx);
                    self.add_notification(
                        title,
                        message
                            .clone()
                            .unwrap_or_else(|| "Waiting for input.".to_owned()),
                        NotificationCategory::Request,
                        NotificationSourceAgent::CLI {
                            agent: *agent,
                            is_ambient: metadata.is_ambient,
                        },
                        NotificationOrigin::CLISession(*terminal_view_id),
                        *terminal_view_id,
                        vec![],
                        metadata.branch,
                        ctx,
                    );
                }
                CLIAgentSessionStatus::Cancelled => {
                    let title = session_context
                        .display_title()
                        .unwrap_or_else(|| format!("{} cancelled", agent.display_name()));
                    let metadata = TerminalViewMetadata::lookup(*terminal_view_id, ctx);
                    self.add_notification(
                        title,
                        "Cancelled by user.".to_owned(),
                        NotificationCategory::Complete,
                        NotificationSourceAgent::CLI {
                            agent: *agent,
                            is_ambient: metadata.is_ambient,
                        },
                        NotificationOrigin::CLISession(*terminal_view_id),
                        *terminal_view_id,
                        vec![],
                        metadata.branch,
                        ctx,
                    );
                }
            },
        }
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        // When a conversation is deleted or removed, clean up its notification and pending artifacts.
        if let BlocklistAIHistoryEvent::DeletedConversation {
            conversation_id, ..
        }
        | BlocklistAIHistoryEvent::RemoveConversation {
            conversation_id, ..
        } = event
        {
            if FeatureFlag::HOANotifications.is_enabled() {
                self.pending_artifacts.remove(conversation_id);
                self.remove_notification_by_source(
                    NotificationOrigin::Conversation(*conversation_id),
                    ctx,
                );
            }
            return;
        }

        // Accumulate artifacts as they arrive during the conversation.
        if let BlocklistAIHistoryEvent::UpdatedConversationArtifacts {
            conversation_id,
            artifact,
            ..
        } = event
        {
            if FeatureFlag::HOANotifications.is_enabled() {
                self.pending_artifacts
                    .entry(*conversation_id)
                    .or_default()
                    .push(artifact.clone());
            }
            return;
        }

        let BlocklistAIHistoryEvent::UpdatedConversationStatus {
            terminal_surface_id,
            conversation_id,
            // We shouldn't trigger toasts when restoring conversations on startup.
            update: ConversationStatusUpdate::Changed { .. },
            ..
        } = event
        else {
            return;
        };

        let ai_history_model = BlocklistAIHistoryModel::as_ref(ctx);
        let Some(updated_conversation) = ai_history_model.conversation(conversation_id) else {
            return;
        };

        if updated_conversation.should_exclude_from_navigation() {
            return;
        }

        self.record_session_activity(*terminal_surface_id);

        let status = updated_conversation.status().clone();
        let latest_query = updated_conversation.latest_user_query();
        if FeatureFlag::HOANotifications.is_enabled() {
            self.handle_history_event_for_mailbox(
                &status,
                *conversation_id,
                latest_query,
                *terminal_surface_id,
                ctx,
            );
            // The new mailbox path handled the event — skip the legacy toast path below.
            return;
        }

        if !status.should_trigger_notification() {
            return;
        }

        if is_terminal_view_visible(*terminal_surface_id, ctx) {
            return;
        }

        let Some((window_id, tab_index)) =
            window_and_tab_idx_id_for_conversation(*conversation_id, ctx)
        else {
            return;
        };

        ctx.emit(AgentManagementEvent::ConversationNeedsAttention {
            window_id,
            tab_index,
            terminal_view_id: *terminal_surface_id,
            conversation_id: *conversation_id,
        });
    }

    fn handle_history_event_for_mailbox(
        &mut self,
        status: &ConversationStatus,
        conversation_id: AIConversationId,
        latest_query: Option<String>,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        let origin = NotificationOrigin::Conversation(conversation_id);

        // If the conversation view is no longer open, don't create notifications for it
        // (there's nothing to navigate to when clicking them).
        if !ActiveAgentViewsModel::as_ref(ctx).is_conversation_open(conversation_id, ctx) {
            self.pending_artifacts.remove(&conversation_id);
            self.remove_notification_by_source(origin, ctx);
            return;
        }

        let title = latest_query.unwrap_or_else(|| "Agent task".to_owned());
        let metadata = TerminalViewMetadata::lookup(terminal_view_id, ctx);
        let oz_agent = NotificationSourceAgent::Oz {
            is_ambient: metadata.is_ambient,
        };

        match status {
            // When the agent resumes its work (or is automatically recovering from a
            // transient failure), clear stale notifications.
            ConversationStatus::InProgress | ConversationStatus::TransientError => {
                self.remove_notification_by_source(origin, ctx);
            }
            ConversationStatus::Success => {
                // Suppress the completion notification when a queued follow-up prompt will
                // auto-send as soon as this conversation finishes. The conversation isn't
                // really in a stopped state, so the notification would be noisy. Pending
                // artifacts are left intact so they roll into the notification fired when the
                // conversation eventually finishes with an empty queue.
                if QueuedQueryModel::as_ref(ctx).has_autofireable_prompt(conversation_id) {
                    return;
                }
                let artifacts = self.flush_pending_artifacts(conversation_id);
                self.add_notification(
                    title,
                    "Task completed.".to_owned(),
                    NotificationCategory::Complete,
                    oz_agent,
                    origin,
                    terminal_view_id,
                    artifacts,
                    metadata.branch,
                    ctx,
                );
            }
            ConversationStatus::Cancelled => {
                let artifacts = self.flush_pending_artifacts(conversation_id);
                self.add_notification(
                    title,
                    "Task was cancelled.".to_owned(),
                    NotificationCategory::Complete,
                    oz_agent,
                    origin,
                    terminal_view_id,
                    artifacts,
                    metadata.branch,
                    ctx,
                );
            }
            ConversationStatus::Blocked { blocked_action } => {
                self.add_notification(
                    title,
                    blocked_action.clone(),
                    NotificationCategory::Request,
                    oz_agent,
                    origin,
                    terminal_view_id,
                    vec![],
                    metadata.branch,
                    ctx,
                );
            }
            ConversationStatus::Error => {
                let artifacts = self.flush_pending_artifacts(conversation_id);
                self.add_notification(
                    title,
                    "Something went wrong.".to_owned(),
                    NotificationCategory::Error,
                    oz_agent,
                    origin,
                    terminal_view_id,
                    artifacts,
                    metadata.branch,
                    ctx,
                );
            }
            // Yielded conversations are still active; mirror the
            // InProgress arm and clear any stale notification for this
            // origin.
            ConversationStatus::WaitingForEvents => {
                self.remove_notification_by_source(origin, ctx);
            }
        }
    }

    /// Removes the existing notification for the given source (if any) and emits an update event.
    fn remove_notification_by_source(
        &mut self,
        origin: NotificationOrigin,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.notifications.remove_by_origin(origin) {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Drains and returns the pending artifacts for a conversation.
    pub(crate) fn flush_pending_artifacts(
        &mut self,
        conversation_id: AIConversationId,
    ) -> Vec<Artifact> {
        self.pending_artifacts
            .remove(&conversation_id)
            .unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    fn add_notification(
        &mut self,
        title: String,
        message: String,
        category: NotificationCategory,
        agent: NotificationSourceAgent,
        origin: NotificationOrigin,
        terminal_view_id: EntityId,
        artifacts: Vec<Artifact>,
        branch: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let is_visible = is_terminal_view_visible(terminal_view_id, ctx);
        let item = NotificationItem::new(
            title,
            message,
            category,
            agent,
            origin,
            is_visible,
            terminal_view_id,
            artifacts,
            branch,
        );

        let id = item.id;
        self.notifications.push(item);
        ctx.emit(AgentManagementEvent::NotificationAdded { id });
    }
}

#[derive(Clone, Debug)]
pub enum AgentManagementEvent {
    /// A Warp-native conversation needs attention and is not visible in the current window/tab.
    ConversationNeedsAttention {
        window_id: WindowId,
        tab_index: usize,
        terminal_view_id: EntityId,
        conversation_id: AIConversationId,
    },
    /// A new notification was added to the persistent notification center.
    NotificationAdded { id: NotificationId },
    /// A notification's read state changed.
    NotificationUpdated,
    /// All notifications were marked as read.
    AllNotificationsMarkedRead,
}

impl ConversationStatus {
    /// Returns true if the updating the conversation with this status should trigger some
    /// notification to the user.
    ///
    /// Exhaustive match so a new `ConversationStatus` variant forces a
    /// deliberate decision about whether it should fire a notification.
    pub fn should_trigger_notification(&self) -> bool {
        match self {
            ConversationStatus::Success
            | ConversationStatus::Blocked { .. }
            | ConversationStatus::Error => true,
            // Streaming hasn't reached a notable state; a recovering or
            // yielded conversation is still active; user-cancellations are
            // self-evident.
            ConversationStatus::InProgress
            | ConversationStatus::TransientError
            | ConversationStatus::WaitingForEvents
            | ConversationStatus::Cancelled => false,
        }
    }
}

fn is_terminal_view_visible(terminal_view_id: EntityId, app: &AppContext) -> bool {
    let Some(active_id) = active_focused_terminal_id(app) else {
        return false;
    };
    active_id == terminal_view_id
        || is_terminal_view_in_same_tab(&active_id, &terminal_view_id, app)
}

fn window_and_tab_idx_id_for_conversation(
    conversation_id: AIConversationId,
    app: &AppContext,
) -> Option<(WindowId, usize)> {
    WorkspaceRegistry::as_ref(app)
        .all_workspaces(app)
        .iter()
        .find_map(|(window_id, workspace_handle)| {
            workspace_handle
                .as_ref(app)
                .tab_views()
                .enumerate()
                .find_map(|(tab_idx, pane_group)| {
                    pane_group
                        .as_ref(app)
                        .terminal_pane_ids()
                        .filter_map(|pane_id| {
                            pane_group
                                .as_ref(app)
                                .terminal_view_from_pane_id(pane_id, app)
                        })
                        .find_map(|terminal_view| {
                            let terminal_view_conversation_id =
                                terminal_view.as_ref(app).active_conversation_id(app)?;
                            (terminal_view_conversation_id == conversation_id)
                                .then_some((*window_id, tab_idx))
                        })
                })
        })
}

/// Per-notification metadata derived from a single [`TerminalView`] lookup. Both fields
/// are read on the same emit path, so we resolve the view once and pass the projection
/// down rather than walking the workspace tree for each.
struct TerminalViewMetadata {
    is_ambient: bool,
    branch: Option<String>,
}

impl TerminalViewMetadata {
    fn lookup(terminal_view_id: EntityId, app: &AppContext) -> Self {
        let Some(terminal_view) = find_terminal_view_by_id(terminal_view_id, app) else {
            return Self {
                is_ambient: false,
                branch: None,
            };
        };
        let view = terminal_view.as_ref(app);
        Self {
            is_ambient: view.is_ambient_agent_session(app),
            branch: view.current_git_branch(app),
        }
    }
}

fn find_terminal_view_by_id(
    terminal_view_id: EntityId,
    app: &AppContext,
) -> Option<ViewHandle<TerminalView>> {
    for (_, workspace_handle) in WorkspaceRegistry::as_ref(app).all_workspaces(app) {
        for pane_group in workspace_handle.as_ref(app).tab_views() {
            let pane_group = pane_group.as_ref(app);
            for pane_id in pane_group.terminal_pane_ids() {
                if let Some(terminal_view) = pane_group.terminal_view_from_pane_id(pane_id, app)
                    && terminal_view.id() == terminal_view_id
                {
                    return Some(terminal_view);
                }
            }
        }
    }
    None
}

fn active_focused_terminal_id(app: &AppContext) -> Option<EntityId> {
    let active_window = app.windows().active_window()?;
    let workspace = app
        .views_of_type::<Workspace>(active_window)
        .and_then(|views| views.first().cloned())?;

    let workspace = workspace.as_ref(app);
    workspace.active_terminal_id(app)
}

#[cfg(test)]
#[path = "agent_management_model_tests.rs"]
mod tests;
