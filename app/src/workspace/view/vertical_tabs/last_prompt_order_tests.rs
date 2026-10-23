use warp_core::features::FeatureFlag;
use warpui::{App, AppContext, EntityId, SingletonEntity as _, TypedActionView as _, ViewHandle};

use super::{TabSlot, cli_status_change_moves_tab, opens_tab_for_user, top_of_unpinned_region};
use crate::pane_group::Direction;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::event::parse_event;
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
    CLIAgentSessionsModel,
};
use crate::workspace::Workspace;
use crate::workspace::action::WorkspaceAction;
use crate::workspace::tab_group::TabGroup;
use crate::workspace::tab_settings::VerticalTabsSortMode;
use crate::workspace::view::tests::{initialize_app, mock_workspace};
use crate::workspace::view::vertical_tabs::status_order::StatusRowKey;

const PINNED: TabSlot = TabSlot {
    is_pinned: true,
    is_grouped: false,
};
const PLAIN: TabSlot = TabSlot {
    is_pinned: false,
    is_grouped: false,
};
const GROUPED: TabSlot = TabSlot {
    is_pinned: false,
    is_grouped: true,
};

#[test]
fn tabs_move_to_the_first_slot_after_the_pinned_tabs() {
    assert_eq!(top_of_unpinned_region(&[PLAIN, PLAIN, PLAIN], 2), Some(0));
    assert_eq!(top_of_unpinned_region(&[PLAIN, PLAIN, PLAIN], 1), Some(0));
    assert_eq!(
        top_of_unpinned_region(&[PINNED, PINNED, PLAIN, PLAIN], 3),
        Some(2)
    );
    // A tab between two groups lands above the first one, never inside it.
    assert_eq!(
        top_of_unpinned_region(&[PINNED, GROUPED, GROUPED, PLAIN], 3),
        Some(1)
    );
}

#[test]
fn some_tabs_stay_where_they_are() {
    // On top already.
    assert_eq!(top_of_unpinned_region(&[PLAIN, PLAIN], 0), None);
    assert_eq!(top_of_unpinned_region(&[PINNED, PLAIN, PLAIN], 1), None);
    // Pinned, or in a tab group (pinned or not).
    assert_eq!(top_of_unpinned_region(&[PINNED, PINNED, PLAIN], 1), None);
    assert_eq!(top_of_unpinned_region(&[PLAIN, GROUPED], 1), None);
    let pinned_group = TabSlot {
        is_pinned: true,
        is_grouped: true,
    };
    assert_eq!(top_of_unpinned_region(&[pinned_group, PLAIN], 0), None);
    // An unpinned tab inside the pinned region (never expected) isn't moved
    // out of it.
    assert_eq!(top_of_unpinned_region(&[PLAIN, PINNED], 0), None);
    // Out of range, or no tabs at all.
    assert_eq!(top_of_unpinned_region(&[PLAIN, PLAIN], 2), None);
    assert_eq!(top_of_unpinned_region(&[PLAIN], usize::MAX), None);
    assert_eq!(top_of_unpinned_region(&[], 0), None);
}

#[test]
fn prompts_and_answers_move_a_tab_and_nothing_else_does() {
    // (was waiting, is waiting, prompt submit, nested run) -> moves
    let cases = [
        // The user's prompt.
        ((false, false, true, false), true),
        ((true, false, true, false), true),
        // A nested run's prompt (e.g. `claude -p` started by the agent).
        ((false, false, true, true), false),
        // Answering (or dismissing) a permission request or question.
        ((true, false, false, false), true),
        ((true, false, false, true), true),
        // A new request, or another one while waiting, doesn't move it: the
        // panel floats the row instead.
        ((false, true, false, false), false),
        ((true, true, false, false), false),
        // Work finishing, failing or being cancelled.
        ((false, false, false, false), false),
        ((false, false, false, true), false),
    ];
    for ((was_waiting, is_waiting, is_prompt_submit, is_nested), moves) in cases {
        assert_eq!(
            cli_status_change_moves_tab(was_waiting, is_waiting, is_prompt_submit, is_nested),
            moves,
            "{:?}",
            (was_waiting, is_waiting, is_prompt_submit, is_nested)
        );
    }
}

#[test]
fn only_new_tab_actions_count_as_the_user_opening_a_tab() {
    assert!(opens_tab_for_user(&WorkspaceAction::AddDefaultTab));
    assert!(opens_tab_for_user(&WorkspaceAction::AddTerminalTab {
        hide_homepage: false
    }));
    assert!(opens_tab_for_user(&WorkspaceAction::AddAgentTab));
    assert!(!opens_tab_for_user(&WorkspaceAction::ActivateTab(0)));
    assert!(!opens_tab_for_user(&WorkspaceAction::ReopenClosedSession));
    assert!(!opens_tab_for_user(
        &WorkspaceAction::SetVerticalTabsSortMode(VerticalTabsSortMode::LastPrompt)
    ));
}

fn tab_ids(workspace: &Workspace) -> Vec<EntityId> {
    workspace
        .tabs
        .iter()
        .map(|tab| tab.pane_group.id())
        .collect()
}

fn position(workspace: &Workspace, pane_group_id: EntityId) -> Option<usize> {
    workspace
        .tabs
        .iter()
        .position(|tab| tab.pane_group.id() == pane_group_id)
}

fn tab_position(app: &App, workspace: &ViewHandle<Workspace>, pane_group_id: EntityId) -> usize {
    workspace.read(app, |workspace, _| {
        position(workspace, pane_group_id).expect("the tab is still open")
    })
}

fn active_tab_id(app: &App, workspace: &ViewHandle<Workspace>) -> EntityId {
    workspace.read(app, |workspace, _| {
        workspace.tabs[workspace.active_tab_index].pane_group.id()
    })
}

fn focused_terminal_view_id(workspace: &Workspace, tab_index: usize, ctx: &AppContext) -> EntityId {
    workspace.tabs[tab_index]
        .pane_group
        .as_ref(ctx)
        .focused_session_view(ctx)
        .expect("each tab starts with a terminal pane")
        .id()
}

fn open_tab_like_the_user(app: &mut App, workspace: &ViewHandle<Workspace>) {
    workspace.update(app, |workspace, ctx| {
        workspace.handle_action(
            &WorkspaceAction::AddTerminalTab {
                hide_homepage: false,
            },
            ctx,
        );
    });
}

/// Three tabs opened programmatically, [0, 1, 2] with tab 2 active, and a
/// Claude Code session in tab `agent_tab_index`. Returns the agent tab's pane
/// group and its terminal view.
fn workspace_with_agent_tab(
    app: &mut App,
    workspace: &ViewHandle<Workspace>,
    agent_tab_index: usize,
) -> (EntityId, EntityId) {
    let (agent_tab, agent_view) = workspace.update(app, |workspace, ctx| {
        workspace.add_terminal_tab(false, ctx);
        workspace.add_terminal_tab(false, ctx);
        assert_eq!(workspace.active_tab_index, 2);
        (
            workspace.tabs[agent_tab_index].pane_group.id(),
            focused_terminal_view_id(workspace, agent_tab_index, ctx),
        )
    });
    CLIAgentSessionsModel::handle(&*app).update(app, |model, ctx| {
        model.set_session(
            agent_view,
            CLIAgentSession {
                agent: CLIAgent::Claude,
                status: CLIAgentSessionStatus::InProgress,
                session_context: CLIAgentSessionContext::default(),
                input_state: CLIAgentInputState::Closed,
                should_auto_toggle_input: false,
                listener: None,
                plugin_version: Some("1.0.0".to_owned()),
                remote_host: None,
                draft_text: None,
                custom_command_prefix: None,
                received_rich_notification: true,
            },
            ctx,
        );
    });
    (agent_tab, agent_view)
}

/// Reports a plugin event of the agent in `terminal_view_id`.
fn emit(app: &mut App, terminal_view_id: EntityId, body: &str) {
    let event = parse_event(Some("warp://cli-agent"), body).expect("valid event");
    CLIAgentSessionsModel::handle(&*app).update(app, |model, ctx| {
        model.update_from_event(terminal_view_id, &event, ctx);
    });
}

const SESSION_START: &str =
    r#"{"v":1,"agent":"claude","event":"session_start","session_id":"outer"}"#;
const PROMPT: &str =
    r#"{"v":1,"agent":"claude","event":"prompt_submit","session_id":"outer","query":"fix it"}"#;
const STOP: &str = r#"{"v":1,"agent":"claude","event":"stop","session_id":"outer"}"#;
const PERMISSION_REQUEST: &str = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"outer","summary":"Run cargo test"}"#;
const QUESTION: &str = r#"{"v":1,"agent":"claude","event":"question_asked","session_id":"outer","summary":"Which one?"}"#;
const TOOL_COMPLETE: &str =
    r#"{"v":1,"agent":"claude","event":"tool_complete","session_id":"outer"}"#;

fn add_task_sync_model(app: &mut App) {
    // Terminal views look up the agent's task when its status changes.
    app.add_singleton_model(
        crate::ai::blocklist::local_agent_task_sync_model::LocalAgentTaskSyncModel::new,
    );
}

#[test]
fn tabs_the_user_opens_go_to_the_top_and_other_tabs_keep_their_place() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(
                crate::tab::vertical_tabs_sort_mode(ctx),
                VerticalTabsSortMode::LastPrompt
            );
            // Tabs opened by other paths (restores, launch configurations,
            // ...) keep their placement and order.
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.active_tab_index, 2);
            let others = tab_ids(workspace);

            // A frozen list shows the new tab right away.
            let pane_id = workspace.tabs[0]
                .pane_group
                .as_ref(ctx)
                .focused_pane_id(ctx);
            workspace
                .vertical_tabs_panel
                .status_list_last_order
                .borrow_mut()
                .push(StatusRowKey {
                    pane_group_id: others[0],
                    pane_id,
                });

            workspace.handle_action(
                &WorkspaceAction::AddTerminalTab {
                    hide_homepage: false,
                },
                ctx,
            );
            let ids = tab_ids(workspace);
            assert_eq!(ids.len(), 4);
            assert_eq!(&ids[1..], &others[..], "the other tabs keep their order");
            assert_eq!(workspace.active_tab_index, 0, "the new tab is active");
            assert!(
                workspace
                    .vertical_tabs_panel
                    .status_list_last_order
                    .borrow()
                    .is_empty()
            );
        });
    });
}

#[test]
fn new_tabs_keep_their_placement_in_the_other_orders() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        for mode in [
            VerticalTabsSortMode::Status,
            VerticalTabsSortMode::GroupByProject,
            VerticalTabsSortMode::Manual,
        ] {
            workspace.update(&mut app, |workspace, ctx| {
                workspace.handle_action(&WorkspaceAction::SetVerticalTabsSortMode(mode), ctx);
            });
            workspace.update(&mut app, |workspace, ctx| {
                workspace.activate_tab(0, ctx);
                let before = tab_ids(workspace);
                workspace.handle_action(
                    &WorkspaceAction::AddTerminalTab {
                        hide_homepage: false,
                    },
                    ctx,
                );
                // Right after the active tab, as before.
                assert_eq!(workspace.active_tab_index, 1, "{mode:?}");
                let mut ids = tab_ids(workspace);
                ids.remove(1);
                assert_eq!(ids, before, "{mode:?}");
            });
        }
    });
}

#[test]
fn prompting_an_agent_moves_its_tab_to_the_top_and_reading_it_does_not() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        add_task_sync_model(&mut app);
        let workspace = mock_workspace(&mut app);
        let (agent_tab, agent_view) = workspace_with_agent_tab(&mut app, &workspace, 1);
        let active_tab = active_tab_id(&app, &workspace);

        emit(&mut app, agent_view, SESSION_START);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);
        emit(&mut app, agent_view, PROMPT);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 0);
        assert_eq!(
            active_tab_id(&app, &workspace),
            active_tab,
            "the active tab stays active"
        );

        // The agent finishing, and the user reading the tab, don't move it.
        open_tab_like_the_user(&mut app, &workspace);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);
        emit(&mut app, agent_view, STOP);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(1, ctx);
        });
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);

        // Prompting again does.
        emit(&mut app, agent_view, PROMPT);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 0);
    });
}

#[test]
fn answering_an_agent_moves_its_tab_to_the_top() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        add_task_sync_model(&mut app);
        let workspace = mock_workspace(&mut app);
        let (agent_tab, agent_view) = workspace_with_agent_tab(&mut app, &workspace, 1);
        emit(&mut app, agent_view, SESSION_START);
        emit(&mut app, agent_view, PROMPT);
        open_tab_like_the_user(&mut app, &workspace);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);

        // Asking for permission leaves the tab in place (the panel floats
        // its row instead); the answer moves it up.
        emit(&mut app, agent_view, PERMISSION_REQUEST);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);
        emit(&mut app, agent_view, TOOL_COMPLETE);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 0);

        // So does dismissing a question, which ends the turn.
        open_tab_like_the_user(&mut app, &workspace);
        emit(&mut app, agent_view, QUESTION);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);
        emit(&mut app, agent_view, STOP);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 0);
    });
}

#[test]
fn a_nested_run_of_the_agent_does_not_move_its_tab() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        add_task_sync_model(&mut app);
        let workspace = mock_workspace(&mut app);
        let (agent_tab, agent_view) = workspace_with_agent_tab(&mut app, &workspace, 1);
        emit(&mut app, agent_view, SESSION_START);
        emit(&mut app, agent_view, PROMPT);
        open_tab_like_the_user(&mut app, &workspace);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);

        // `claude -p` started by the agent during its turn.
        emit(
            &mut app,
            agent_view,
            r#"{"v":1,"agent":"claude","event":"session_start","session_id":"nested"}"#,
        );
        emit(
            &mut app,
            agent_view,
            r#"{"v":1,"agent":"claude","event":"prompt_submit","session_id":"nested","query":"summarize"}"#,
        );
        assert_eq!(tab_position(&app, &workspace, agent_tab), 1);
    });
}

#[test]
fn pinned_and_grouped_tabs_stay_in_place() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);
    let _pinned_guard = FeatureFlag::PinnedTabs.override_enabled(true);
    let _grouped_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        add_task_sync_model(&mut app);
        let workspace = mock_workspace(&mut app);
        let (agent_tab, agent_view) = workspace_with_agent_tab(&mut app, &workspace, 2);
        workspace.update(&mut app, |workspace, _| {
            workspace.tabs[0].pinned = true;
        });
        emit(&mut app, agent_view, SESSION_START);
        emit(&mut app, agent_view, PROMPT);
        assert_eq!(
            tab_position(&app, &workspace, agent_tab),
            1,
            "right after the pinned tab"
        );

        // A tab in a tab group stays with its group.
        workspace.update(&mut app, |workspace, _| {
            let group = TabGroup::new();
            let group_id = group.id;
            workspace.tab_groups.insert(group_id, group);
            workspace.tabs[1].group_id = Some(group_id);
            workspace.tabs[2].group_id = Some(group_id);
            workspace.tabs.swap(1, 2);
        });
        assert_eq!(tab_position(&app, &workspace, agent_tab), 2);
        emit(&mut app, agent_view, PROMPT);
        assert_eq!(tab_position(&app, &workspace, agent_tab), 2);
    });
}

#[test]
fn prompts_keep_the_tab_order_in_the_other_orders() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        add_task_sync_model(&mut app);
        let workspace = mock_workspace(&mut app);
        let (agent_tab, agent_view) = workspace_with_agent_tab(&mut app, &workspace, 1);
        emit(&mut app, agent_view, SESSION_START);
        for mode in [
            VerticalTabsSortMode::Status,
            VerticalTabsSortMode::GroupByProject,
            VerticalTabsSortMode::Manual,
        ] {
            workspace.update(&mut app, |workspace, ctx| {
                workspace.handle_action(&WorkspaceAction::SetVerticalTabsSortMode(mode), ctx);
            });
            emit(&mut app, agent_view, PROMPT);
            emit(&mut app, agent_view, PERMISSION_REQUEST);
            emit(&mut app, agent_view, TOOL_COMPLETE);
            assert_eq!(tab_position(&app, &workspace, agent_tab), 1, "{mode:?}");
        }
    });
}

#[test]
fn splitting_or_reopening_a_tab_moves_it_to_the_top() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let first_tab = workspace.read(&app, |workspace, _| workspace.tabs[0].pane_group.id());
        open_tab_like_the_user(&mut app, &workspace);
        open_tab_like_the_user(&mut app, &workspace);
        assert_eq!(tab_position(&app, &workspace, first_tab), 2);

        let pane_group = workspace.read(&app, |workspace, _| workspace.tabs[2].pane_group.clone());
        pane_group.update(&mut app, |pane_group, ctx| {
            pane_group.add_terminal_pane(Direction::Right, None, ctx);
        });
        assert_eq!(tab_position(&app, &workspace, first_tab), 0);

        // Reopening a closed tab puts it on top too.
        let closed_tab = workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(0, ctx);
            let tab = workspace.tabs.remove(2);
            let closed_tab = tab.pane_group.id();
            workspace.restore_closed_tab(2, tab, ctx);
            closed_tab
        });
        assert_eq!(tab_position(&app, &workspace, closed_tab), 0);
        assert_eq!(active_tab_id(&app, &workspace), closed_tab);
        assert_eq!(tab_position(&app, &workspace, first_tab), 1);
    });
}
