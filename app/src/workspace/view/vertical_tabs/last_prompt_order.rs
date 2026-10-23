//! The last-prompt order of the vertical tabs panel (fork feature). The panel
//! shows the workspace's tab order, and a tab moves to the top of it when the
//! user opens it, splits it or reopens it, prompts an agent in it, or answers
//! an agent that was waiting on them. Reading a tab, running a command in a
//! plain shell or an agent finishing never moves it. The tab order is saved
//! with the app state, so the order survives restarts as it is.
//!
//! Only the hooks here move tabs: tabs restored at startup or with a window,
//! opened by a launch configuration, dragged in from another window or
//! opened by any other path keep their normal placement and order.

use std::collections::{HashMap, HashSet};

use warp_core::features::FeatureFlag;
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext, ViewHandle};

use super::any_workspace_pane_being_dragged;
use crate::pane_group::PaneGroup;
use crate::tab::{TabData, vertical_tabs_sort_mode};
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModelEvent};
use crate::workspace::Workspace;
use crate::workspace::action::WorkspaceAction;
use crate::workspace::cross_window_tab_drag::CrossWindowTabDrag;
use crate::workspace::tab_settings::VerticalTabsSortMode;

/// More terminal views than any workspace has: past this, the set of
/// sessions waiting for an answer is reset rather than grown.
const MAX_TRACKED_WAITING_SESSIONS: usize = 1024;

/// What the last-prompt order remembers between events.
#[derive(Default)]
pub(super) struct LastPromptOrderState {
    /// Terminal views whose CLI agent session was last seen waiting for the
    /// user's answer, so that its next status counts as the answer.
    waiting_terminal_views: HashSet<EntityId>,
    /// How many visible panes each tab had when last seen, by pane group, so
    /// that a pane added to a tab (a split, a reopened pane) is noticed.
    pane_counts: HashMap<EntityId, usize>,
}

/// A tab's place in the tab order, as far as moving it to the top goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TabSlot {
    /// In the pinned region: pinned itself, or in a pinned group.
    pub is_pinned: bool,
    /// In a tab group, whose tabs stay together.
    pub is_grouped: bool,
}

/// Where the tab at `from` goes when it moves to the top: the first slot
/// after the pinned tabs. `None` when it stays where it is: it is pinned or in
/// a tab group, it is on top already, or `from` is out of range.
pub(super) fn top_of_unpinned_region(tabs: &[TabSlot], from: usize) -> Option<usize> {
    let tab = tabs.get(from)?;
    if tab.is_pinned || tab.is_grouped {
        return None;
    }
    let top = tabs.iter().take_while(|tab| tab.is_pinned).count();
    (from > top).then_some(top)
}

/// Whether a CLI agent session's status change moves its tab to the top: the
/// user prompted the agent (and not a nested run of the agent inside the
/// session), or the session stopped waiting for the user's answer (the user
/// answered or dismissed the permission request or question).
pub(super) fn cli_status_change_moves_tab(
    was_waiting_for_answer: bool,
    is_waiting_for_answer: bool,
    is_prompt_submit: bool,
    is_from_nested_session: bool,
) -> bool {
    let is_users_prompt = is_prompt_submit && !is_from_nested_session;
    is_users_prompt || (was_waiting_for_answer && !is_waiting_for_answer)
}

/// Whether `action` opens a tab the user asked for: Cmd+T, the new tab button
/// and menu, and the new-session items of the command palette. Other paths
/// that open tabs (restores, launch configurations, opening a file or a
/// conversation, ...) don't count.
pub(super) fn opens_tab_for_user(action: &WorkspaceAction) -> bool {
    matches!(
        action,
        WorkspaceAction::AddDefaultTab
            | WorkspaceAction::AddTerminalTab { .. }
            | WorkspaceAction::AddTabWithShell { .. }
            | WorkspaceAction::AddGetStartedTab
            | WorkspaceAction::AddAmbientAgentTab
            | WorkspaceAction::AddAgentTab
            | WorkspaceAction::AddDockerSandboxTab
            | WorkspaceAction::NewTabInAgentMode { .. }
            | WorkspaceAction::SelectTabConfig(_)
    )
}

/// The tabs that were open before an action that opens tabs for the user, so
/// that the tabs it opened can be told apart afterwards.
pub(in crate::workspace::view) struct TabsBeforeAction(HashSet<EntityId>);

fn is_last_prompt_order_active(ctx: &AppContext) -> bool {
    vertical_tabs_sort_mode(ctx) == VerticalTabsSortMode::LastPrompt
}

impl Workspace {
    /// Call before handling `action`, and hand the result to
    /// [`Self::move_tabs_opened_by_action_to_top`] once it is handled. `None`
    /// unless the action opens tabs for the user in the last-prompt order.
    pub(in crate::workspace::view) fn tabs_before_new_tab_action(
        &self,
        action: &WorkspaceAction,
        ctx: &AppContext,
    ) -> Option<TabsBeforeAction> {
        (opens_tab_for_user(action) && is_last_prompt_order_active(ctx))
            .then(|| TabsBeforeAction(self.tabs.iter().map(|tab| tab.pane_group.id()).collect()))
    }

    /// Moves the tabs that the action opened to the top, keeping their
    /// order. An action that opened none (e.g. one that opened a dialog
    /// instead) moves nothing.
    pub(in crate::workspace::view) fn move_tabs_opened_by_action_to_top(
        &mut self,
        before: Option<TabsBeforeAction>,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(TabsBeforeAction(before)) = before else {
            return;
        };
        let opened: Vec<EntityId> = self
            .tabs
            .iter()
            .map(|tab| tab.pane_group.id())
            .filter(|pane_group_id| !before.contains(pane_group_id))
            .collect();
        self.remember_pane_counts(ctx);
        self.move_tabs_to_top_for_user(&opened, ctx);
    }

    /// Moves a tab the user reopened (undo close) to the top.
    pub(in crate::workspace::view) fn move_reopened_tab_to_top(
        &mut self,
        pane_group_id: EntityId,
        ctx: &mut ViewContext<Self>,
    ) {
        self.remember_pane_counts(ctx);
        self.move_tabs_to_top_for_user(&[pane_group_id], ctx);
    }

    /// Call for every event of a tab's pane group: moves the tab to the top
    /// when a pane was added to it (a split, a reopened pane, a pane dropped
    /// into it). The first event of a tab only learns its pane count.
    pub(in crate::workspace::view) fn note_pane_group_event_for_last_prompt_order(
        &mut self,
        pane_group: &ViewHandle<PaneGroup>,
        ctx: &mut ViewContext<Self>,
    ) {
        let pane_group_id = pane_group.id();
        if !self
            .tabs
            .iter()
            .any(|tab| tab.pane_group.id() == pane_group_id)
        {
            return;
        }
        let pane_count = pane_group.as_ref(ctx).visible_pane_ids().len();
        let previous = self
            .vertical_tabs_panel
            .last_prompt_order
            .pane_counts
            .insert(pane_group_id, pane_count);
        match previous {
            // Also learn the counts of other tabs not seen yet, and forget
            // closed ones.
            None => self.remember_pane_counts(ctx),
            Some(previous) if pane_count > previous => {
                self.move_tabs_to_top_for_user(&[pane_group_id], ctx);
            }
            Some(_) => {}
        }
    }

    /// Call for every CLI agent session event of a terminal view in this
    /// workspace: moves its tab to the top when the user prompts the agent
    /// or answers it.
    pub(in crate::workspace::view) fn note_cli_agent_event_for_last_prompt_order(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                status,
                is_prompt_submit,
                session_context,
                ..
            } => {
                let is_waiting = matches!(status, CLIAgentSessionStatus::Blocked { .. });
                let waiting = &mut self
                    .vertical_tabs_panel
                    .last_prompt_order
                    .waiting_terminal_views;
                let was_waiting = if is_waiting {
                    if waiting.len() >= MAX_TRACKED_WAITING_SESSIONS {
                        waiting.clear();
                    }
                    !waiting.insert(*terminal_view_id)
                } else {
                    waiting.remove(terminal_view_id)
                };
                if !cli_status_change_moves_tab(
                    was_waiting,
                    is_waiting,
                    *is_prompt_submit,
                    session_context.query_from_other_session,
                ) {
                    return;
                }
                if let Some(pane_group_id) = self.tab_showing_terminal_view(*terminal_view_id, ctx)
                {
                    self.move_tabs_to_top_for_user(&[pane_group_id], ctx);
                }
            }
            CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            } => {
                self.vertical_tabs_panel
                    .last_prompt_order
                    .waiting_terminal_views
                    .remove(terminal_view_id);
            }
            CLIAgentSessionsModelEvent::Started { .. }
            | CLIAgentSessionsModelEvent::InputSessionChanged { .. }
            | CLIAgentSessionsModelEvent::SessionUpdated { .. } => {}
        }
    }

    /// The pane group of the tab that shows `terminal_view_id` in one of its
    /// visible panes.
    fn tab_showing_terminal_view(
        &self,
        terminal_view_id: EntityId,
        ctx: &AppContext,
    ) -> Option<EntityId> {
        self.tabs
            .iter()
            .find(|tab| {
                tab.pane_group
                    .as_ref(ctx)
                    .visible_terminal_views(ctx)
                    .iter()
                    .any(|view| view.id() == terminal_view_id)
            })
            .map(|tab| tab.pane_group.id())
    }

    /// Learns the pane count of every tab not seen yet, and forgets the tabs
    /// that are no longer in this workspace.
    fn remember_pane_counts(&mut self, ctx: &AppContext) {
        let current: Vec<(EntityId, usize)> = self
            .tabs
            .iter()
            .map(|tab| {
                (
                    tab.pane_group.id(),
                    tab.pane_group.as_ref(ctx).visible_pane_ids().len(),
                )
            })
            .collect();
        let pane_counts = &mut self.vertical_tabs_panel.last_prompt_order.pane_counts;
        pane_counts.retain(|pane_group_id, _| current.iter().any(|(id, _)| id == pane_group_id));
        for (pane_group_id, pane_count) in current {
            pane_counts.entry(pane_group_id).or_insert(pane_count);
        }
    }

    fn last_prompt_tab_slot(&self, tab: &TabData) -> TabSlot {
        TabSlot {
            is_pinned: self.is_tab_effectively_pinned(tab),
            is_grouped: FeatureFlag::GroupedTabs.is_enabled()
                && tab
                    .group_id
                    .is_some_and(|group_id| self.tab_groups.contains_key(&group_id)),
        }
    }

    /// Whether the user is in the middle of something that refers to tabs by
    /// their position (a tab's menu, a rename, a drag), which moving tabs
    /// would point at the wrong tab.
    fn is_tab_order_in_use(&self, ctx: &AppContext) -> bool {
        self.show_tab_right_click_menu.is_some()
            || self.show_tab_selection_right_click_menu.is_some()
            || self.current_workspace_state.is_tab_being_renamed()
            || self.hovered_tab_index.is_some()
            || self
                .tabs
                .iter()
                .any(|tab| tab.draggable_state.is_dragging())
            || any_workspace_pane_being_dragged(self, ctx)
            || (ctx.has_singleton_model::<CrossWindowTabDrag>()
                && CrossWindowTabDrag::as_ref(ctx).is_active())
    }

    /// Moves the tabs of `pane_group_ids` to the top of the unpinned region,
    /// in that order, after a user's own action, and shows the new order
    /// right away even while the pointer holds the list still. Pinned and
    /// grouped tabs stay where they are. The active tab stays active, and the
    /// new order is saved like any other tab move. Does nothing unless the
    /// last-prompt order is active.
    fn move_tabs_to_top_for_user(
        &mut self,
        pane_group_ids: &[EntityId],
        ctx: &mut ViewContext<Self>,
    ) {
        if pane_group_ids.is_empty() || !is_last_prompt_order_active(ctx) {
            return;
        }
        if self.is_tab_order_in_use(ctx) {
            log::debug!("Not moving tabs to the top while the tab order is in use");
            return;
        }
        let mut moved = false;
        // Bottom first, so that each one lands above the ones moved before.
        for pane_group_id in pane_group_ids.iter().rev() {
            let Some(from) = self
                .tabs
                .iter()
                .position(|tab| tab.pane_group.id() == *pane_group_id)
            else {
                continue;
            };
            let slots: Vec<TabSlot> = self
                .tabs
                .iter()
                .map(|tab| self.last_prompt_tab_slot(tab))
                .collect();
            let Some(to) = top_of_unpinned_region(&slots, from) else {
                continue;
            };
            self.move_tab_to_index(from, to, ctx);
            moved = true;
        }
        // The user's own action: show it now, even if the list is frozen
        // under the pointer (a frozen list would keep the tab where it was,
        // or put a new tab at the bottom). Later changes freeze again.
        self.vertical_tabs_panel.release_status_list_freeze_once();
        if moved {
            ctx.dispatch_global_action("workspace:save_app", ());
        }
        ctx.notify();
    }
}

#[cfg(test)]
#[path = "last_prompt_order_tests.rs"]
mod tests;
