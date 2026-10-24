use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use warp_core::features::FeatureFlag;
use warpui::r#async::Timer;
use warpui::platform::WindowStyle;
use warpui::{App, AppContext, EntityId, SingletonEntity as _, TypedActionView as _, ViewHandle};

use super::{TabSlot, cli_status_change_moves_tab, opens_tab_for_user, top_of_unpinned_region};
use crate::GlobalResourceHandles;
use crate::app_state::WindowSnapshot;
use crate::pane_group::Direction;
use crate::root_view::NewWorkspaceSource;
use crate::tab::InterruptedSessionAction;
use crate::terminal::cli_agent_sessions::event::parse_event;
use crate::terminal::cli_agent_sessions::restore::PersistedCLIAgentSession;
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
    CLIAgentSessionsModel,
};
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::{CLIAgent, TerminalView};
use crate::workspace::Workspace;
use crate::workspace::action::{TabContextMenuAnchor, WorkspaceAction};
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

const SESSION_A: &str = "0b1f6e2a-7c3d-4e5f-8a9b-0c1d2e3f4a5b";
const SESSION_B: &str = "1c2a7f3b-8d4e-4f6a-9b0c-1d2e3f4a5b6c";
const SESSION_C: &str = "2d3b8a4c-9e5f-4a7b-8c1d-2e3f4a5b6c7d";
const SESSION_D: &str = "3e4c9b5d-0f6a-4b8c-9d2e-3f4a5b6c7d8e";

/// A restored agent session record. One that didn't start from its pane's
/// own shell (`launched_locally` false) is only offered, never resumed on its
/// own.
fn agent_record(session_id: &str, launched_locally: bool) -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        agent: CLIAgent::Claude,
        session_id: Some(session_id.to_owned()),
        launch_cwd: Some("/Users/me/repo".to_owned()),
        title: Some("fix the login bug".to_owned()),
        transcript_path: None,
        status: None,
        remote_host: None,
        launched_locally,
        evidently_interactive: true,
        updated_at_unix_ms: 1_000,
        last_activity_at_unix_ms: None,
        session_name: None,
        awaiting_first_prompt: false,
    }
}

fn enable_auto_resume(app: &mut App) {
    use ::settings::Setting as _;

    GeneralSettings::handle(&*app).update(app, |settings, ctx| {
        settings
            .auto_resume_cli_agent_sessions
            .set_value(true, ctx)
            .expect("the setting can be changed");
    });
}

fn terminal_of_tab(
    app: &App,
    workspace: &ViewHandle<Workspace>,
    tab_index: usize,
) -> ViewHandle<TerminalView> {
    workspace.read(app, |workspace, ctx| {
        workspace.tabs[tab_index]
            .pane_group
            .as_ref(ctx)
            .focused_session_view(ctx)
            .expect("each tab starts with a terminal pane")
    })
}

/// Four tabs, the last one active, whose panes had the agent sessions of
/// `records` (in tab order) when Warp quit, as the saved window has them.
fn saved_window_with_agent_sessions(
    app: &mut App,
    records: [PersistedCLIAgentSession; 4],
) -> WindowSnapshot {
    let workspace = mock_workspace(app);
    workspace.update(app, |workspace, ctx| {
        for _ in 0..3 {
            workspace.add_terminal_tab(false, ctx);
        }
        assert_eq!(workspace.active_tab_index, 3);
    });
    for (tab_index, record) in records.into_iter().enumerate() {
        terminal_of_tab(app, &workspace, tab_index).update(app, |view, ctx| {
            view.restore_cli_agent_session(record, ctx);
        });
    }
    workspace.update(app, |workspace, ctx| {
        workspace.snapshot(ctx.window_id(), false, ctx)
    })
}

fn restored_workspace(app: &mut App, window_snapshot: WindowSnapshot) -> ViewHandle<Workspace> {
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

/// The session id of each tab's agent session record, in tab order.
fn session_id_of_each_tab(app: &App, workspace: &ViewHandle<Workspace>) -> Vec<Option<String>> {
    workspace.read(app, |workspace, ctx| {
        workspace
            .tabs
            .iter()
            .map(|tab| {
                tab.pane_group
                    .as_ref(ctx)
                    .terminal_views(ctx)
                    .iter()
                    .find_map(|view| view.as_ref(ctx).cli_agent_session_for_snapshot())
                    .and_then(|record| record.session_id)
            })
            .collect()
    })
}

fn active_tab_session_id(app: &App, workspace: &ViewHandle<Workspace>) -> Option<String> {
    let active_tab_index = workspace.read(app, |workspace, _| workspace.active_tab_index);
    session_id_of_each_tab(app, workspace)
        .get(active_tab_index)
        .cloned()
        .flatten()
}

fn is_resuming(app: &App, workspace: &ViewHandle<Workspace>, tab_index: usize) -> bool {
    terminal_of_tab(app, workspace, tab_index)
        .read(app, |view, _| view.is_resuming_cli_agent_session())
}

fn ids(session_ids: &[&str]) -> Vec<Option<String>> {
    session_ids
        .iter()
        .map(|session_id| Some((*session_id).to_owned()))
        .collect()
}

#[test]
fn tabs_whose_agent_resumes_after_a_restore_go_to_the_top_in_their_order() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        // Tabs B and D resume on their own; A and C are only offered.
        let saved = saved_window_with_agent_sessions(
            &mut app,
            [
                agent_record(SESSION_A, false),
                agent_record(SESSION_B, true),
                agent_record(SESSION_C, false),
                agent_record(SESSION_D, true),
            ],
        );
        enable_auto_resume(&mut app);

        let restored = restored_workspace(&mut app, saved);
        assert_eq!(
            session_id_of_each_tab(&app, &restored),
            ids(&[SESSION_B, SESSION_D, SESSION_A, SESSION_C])
        );
        assert!(is_resuming(&app, &restored, 0));
        assert!(is_resuming(&app, &restored, 1));
        assert!(!is_resuming(&app, &restored, 2));
        assert_eq!(
            active_tab_session_id(&app, &restored).as_deref(),
            Some(SESSION_D),
            "the active tab stays active"
        );
    });
}

#[test]
fn restored_tabs_keep_their_order_when_nothing_resumes_or_in_the_other_orders() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let saved = saved_window_with_agent_sessions(
            &mut app,
            [
                agent_record(SESSION_A, false),
                agent_record(SESSION_B, true),
                agent_record(SESSION_C, false),
                agent_record(SESSION_D, true),
            ],
        );

        // Without auto-resume, every session is only offered.
        let restored = restored_workspace(&mut app, saved.clone());
        let saved_order = ids(&[SESSION_A, SESSION_B, SESSION_C, SESSION_D]);
        assert_eq!(session_id_of_each_tab(&app, &restored), saved_order);
        assert!(!is_resuming(&app, &restored, 1));

        enable_auto_resume(&mut app);
        restored.update(&mut app, |workspace, ctx| {
            workspace.handle_action(
                &WorkspaceAction::SetVerticalTabsSortMode(VerticalTabsSortMode::Manual),
                ctx,
            );
        });
        let restored = restored_workspace(&mut app, saved);
        assert_eq!(session_id_of_each_tab(&app, &restored), saved_order);
        assert!(is_resuming(&app, &restored, 1));
        assert!(is_resuming(&app, &restored, 3));
    });
}

/// Yields to the executor until the tab order is `expected`, for at most a
/// few seconds: the user's resume moves its tab once the event it came with
/// is handled.
async fn wait_for_tab_order(app: &App, workspace: &ViewHandle<Workspace>, expected: &[EntityId]) {
    for _ in 0..60 {
        if workspace.read(app, |workspace, _| tab_ids(workspace) == expected) {
            return;
        }
        Timer::after(Duration::from_millis(50)).await;
    }
}

#[test]
fn resuming_an_agent_session_moves_its_tab_to_the_top() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    let _status_order_guard = FeatureFlag::VerticalTabsStatusOrder.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let tabs = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            tab_ids(workspace)
        });
        let active_tab = active_tab_id(&app, &workspace);
        let terminals: Vec<_> = (0..3)
            .map(|tab_index| terminal_of_tab(&app, &workspace, tab_index))
            .collect();
        // Each session is restored right before it's resumed: its launch
        // directory doesn't exist here, so once the check of the directory
        // reports back (when the test yields), it can only be copied. That
        // check also keeps the resume from ever being sent to the shell.
        let restore = |app: &mut App, tab_index: usize, session_id: &str| {
            terminals[tab_index].update(app, |view, ctx| {
                view.restore_cli_agent_session(agent_record(session_id, true), ctx);
            });
        };

        // The Resume of tab 1's row menu in the sidebar. The menu is still
        // open while its item's action runs, and closes right after.
        restore(&mut app, 1, SESSION_B);
        let terminal_view_id = terminals[1].id();
        workspace.update(&mut app, |workspace, ctx| {
            workspace.show_tab_right_click_menu =
                Some((1, TabContextMenuAnchor::VerticalTabsKebab));
            workspace.handle_action(
                &WorkspaceAction::InterruptedCLIAgentSession {
                    terminal_view_id,
                    action: InterruptedSessionAction::Resume,
                },
                ctx,
            );
        });
        workspace.update(&mut app, |workspace, _| {
            workspace.show_tab_right_click_menu = None;
        });
        let expected = [tabs[1], tabs[0], tabs[2]];
        wait_for_tab_order(&app, &workspace, &expected).await;
        workspace.read(&app, |workspace, _| {
            assert_eq!(tab_ids(workspace), expected)
        });
        assert_eq!(
            active_tab_id(&app, &workspace),
            active_tab,
            "the active tab stays active"
        );

        // The Resume of tab 2's banner.
        restore(&mut app, 2, SESSION_C);
        terminals[2].update(&mut app, |view, ctx| {
            view.resume_restored_cli_agent_session_with_feedback(ctx);
        });
        let expected = [tabs[2], tabs[1], tabs[0]];
        wait_for_tab_order(&app, &workspace, &expected).await;
        workspace.read(&app, |workspace, _| {
            assert_eq!(tab_ids(workspace), expected)
        });

        // Resuming all moves the tabs it resumes, here only tab 0: the other
        // sessions can only be copied by now.
        restore(&mut app, 0, SESSION_A);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(&WorkspaceAction::ResumeAllCLIAgentSessions, ctx);
        });
        let expected = [tabs[0], tabs[2], tabs[1]];
        workspace.read(&app, |workspace, _| {
            assert_eq!(tab_ids(workspace), expected)
        });

        // A Resume that can't go ahead (tab 1's session can only be copied by
        // now) doesn't move its tab.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(
                &WorkspaceAction::InterruptedCLIAgentSession {
                    terminal_view_id,
                    action: InterruptedSessionAction::Resume,
                },
                ctx,
            );
        });
        Timer::after(Duration::from_millis(250)).await;
        workspace.read(&app, |workspace, _| {
            assert_eq!(tab_ids(workspace), expected)
        });
    });
}
