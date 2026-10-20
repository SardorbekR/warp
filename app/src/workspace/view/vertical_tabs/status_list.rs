//! Renders the vertical tabs panel when rows are ordered by status (fork
//! feature): a flat, status-sorted list of every pane, or the same rows grouped
//! by project. Manual order keeps using the upstream renderer in the parent
//! module.
//!
//! Row data is gathered once per render, sorted by the pure helpers in
//! [`super::status_order`], and rendered through the shared pane-row shell so
//! hover, selection, rename, context menus and the detail sidecar behave like
//! the manual list.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use instant::Instant;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use settings::Setting as _;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::{ColorScheme, Fill as WarpThemeFill, WarpTheme};
use warpui::elements::{
    Align, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DispatchEventResult,
    DropShadow, Element, Empty, EventHandler, Expanded, Flex, Hoverable, Icon, MainAxisSize,
    MouseInBehavior, MouseStateHandle, Padding, ParentElement, Radius, SavePosition, Shrinkable,
    Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId, EventContext, SingletonEntity, TaskId, ViewHandle};

use super::status_order::{
    AttentionBucket, FreezeInputs, GroupHeaderText, POINTER_IDLE_CAP, POINTER_IDLE_RELEASE,
    StatusRowKey, StatusSortInput, activity_age, compact_age_label, group_by_project,
    group_header_text, is_order_frozen, project_name, project_palette_color, resolve_display_order,
    row_attention_bucket, row_display_status, status_sorted_indices,
};
use super::{
    PaneProps, PaneRowBadgeMouseStates, PaneRowState, StatusRowClick, TerminalPrimaryLineData,
    TerminalPrimaryLineFont, TypedPane, VerticalTabsPanelState, agent_tab_text_preference,
    any_workspace_pane_being_dragged, pane_matches_query, pane_row_background,
    preferred_agent_tab_titles, render_inline_tab_rename_editor, render_pane_row_element,
    render_synced_inputs_indicator, resolve_icon_with_status_variant,
    resolved_terminal_working_directory, row_shows_synced_inputs_indicator, terminal_agent_text,
    terminal_primary_line_data, terminal_title_fallback_font,
};
use crate::ai::agent::conversation::{ConversationStatus, StatusColorStyle};
use crate::ai::agent_management::AgentNotificationsModel;
use crate::appearance::Appearance;
use crate::editor::EditorView;
use crate::pane_group::PaneGroup;
use crate::tab::tab_position_id;
use crate::terminal::TerminalView;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::view::TerminalViewState;
use crate::ui_components::icon_with_status::IconWithStatusVariant;
use crate::workspace::action::WorkspaceAction;
use crate::workspace::tab_settings::{
    TabSettings, VerticalTabsDisplayGranularity, VerticalTabsSortMode,
};
use crate::workspace::{PaneViewLocator, Workspace};

/// Size of the status glyph at the end of each row.
const STATUS_GLYPH_SIZE: f32 = 16.;
const ROW_TITLE_FONT_SIZE: f32 = 13.;
const ROW_META_FONT_SIZE: f32 = 12.;
/// Width of the age slot, so that project tags line up across rows.
const ROW_AGE_WIDTH: f32 = 28.;
const PROJECT_TAG_FONT_SIZE: f32 = 11.;
const PROJECT_TAG_MAX_WIDTH: f32 = 78.;
const GROUP_HEADER_FONT_SIZE: f32 = 12.;
const ROW_SPACING: f32 = 2.;
const ERROR_ICON_SIZE: f32 = 12.;
/// Inset of the list, rows and headers from the panel's edges.
const LIST_HORIZONTAL_PADDING: f32 = 10.;
/// Every status row is fully rounded (hover, selection and resting alike).
pub(super) const STATUS_ROW_CORNER_RADIUS: f32 = 8.;

const RUNNING_GLYPH_PATH: &str = "bundled/svg/fork-status-running.svg";
const NEEDS_YOU_GLYPH_PATH: &str = "bundled/svg/fork-status-needs-you.svg";
const DONE_UNREAD_GLYPH_PATH: &str = "bundled/svg/fork-status-done-unread.svg";
const DONE_READ_GLYPH_PATH: &str = "bundled/svg/fork-status-done-read.svg";

/// "Needs you" amber, fixed so it doesn't depend on the theme's ANSI yellow.
const NEEDS_YOU_AMBER_DARK: ColorU = ColorU {
    r: 0xF5,
    g: 0xB1,
    b: 0x3D,
    a: 0xFF,
};
const NEEDS_YOU_AMBER_LIGHT: ColorU = ColorU {
    r: 0xD9,
    g: 0x8A,
    b: 0x0F,
    a: 0xFF,
};
/// A darker amber for "k needs you" text on light themes, so it stays
/// readable.
const NEEDS_YOU_TEXT_LIGHT: ColorU = ColorU {
    r: 0x9A,
    g: 0x55,
    b: 0x00,
    a: 0xFF,
};

/// Everything needed to sort and render one row, gathered once per render.
struct StatusRow<'a> {
    key: StatusRowKey,
    tab_index: usize,
    props: PaneProps<'a>,
    status: Option<ConversationStatus>,
    terminal_view_id: Option<EntityId>,
    is_unread: bool,
    is_errored: bool,
    /// Set when the pane holds an agent session that was interrupted (e.g.
    /// by a crash or restart) and can be resumed from the pane.
    interrupted: Option<InterruptedSession>,
    project: Option<String>,
    age: Option<Duration>,
    bucket: AttentionBucket,
    fallback_rank: (usize, usize),
    title: StatusRowTitle,
    /// Whether this row carries the tab's `tab_position_id`, so that
    /// scrolling to a tab lands on the row of its focused pane.
    saves_tab_position: bool,
}

impl StatusRow<'_> {
    fn sort_input(&self) -> StatusSortInput<'_> {
        StatusSortInput {
            key: self.key,
            bucket: self.bucket,
            is_unread: self.is_unread,
            age: self.age,
            fallback_rank: self.fallback_rank,
            project: self.project.as_deref(),
        }
    }
}

/// What a row shows for an interrupted agent session.
#[derive(Clone, Copy)]
struct InterruptedSession {
    /// The session ran over SSH, so it can't be resumed from this machine.
    is_remote: bool,
}

enum StatusRowTitle {
    /// The inline rename editor for the tab or pane.
    Editor(ViewHandle<EditorView>),
    Text {
        text: String,
        font: TerminalPrimaryLineFont,
    },
}

struct SectionHeader {
    text: GroupHeaderText,
}

/// A run of rows, optionally preceded by a project header.
struct ListSection {
    header: Option<SectionHeader>,
    row_indices: Vec<usize>,
}

/// Renders the status-ordered list. `mode` is the effective sort mode and is
/// never `Manual` in practice (the caller renders manual order itself); a
/// `Manual` value is rendered as the flat list.
pub(super) fn render_status_ordered_list(
    state: &VerticalTabsPanelState,
    workspace: &Workspace,
    mode: VerticalTabsSortMode,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let rows = collect_status_rows(state, workspace, app);

    if rows.is_empty() {
        if state.search_query.is_empty() {
            return Empty::new().finish();
        }
        return Container::new(
            Text::new_inline(
                "No tabs match your search.",
                appearance.ui_font_family(),
                12.,
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
        )
        .with_padding(Padding::uniform(12.))
        .finish();
    }

    let (sections, display_keys) = layout_sections(state, workspace, mode, &rows, app);
    store_last_order(state, display_keys);

    // Rows are moved into their elements in display order.
    let mut slots: Vec<Option<StatusRow<'_>>> = rows.into_iter().map(Some).collect();
    let show_project_tag = !matches!(mode, VerticalTabsSortMode::GroupByProject);
    let palette = StatusPalette::new(theme);
    let mut list = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(ROW_SPACING);
    for (section_index, section) in sections.into_iter().enumerate() {
        if let Some(header) = section.header {
            list.add_child(render_group_header(
                &header,
                section_index == 0,
                &palette,
                appearance,
            ));
        }
        for row_index in section.row_indices {
            let Some(row) = slots.get_mut(row_index).and_then(Option::take) else {
                continue;
            };
            list.add_child(render_status_row(
                row,
                show_project_tag,
                &palette,
                appearance,
                app,
            ));
        }
    }

    Container::new(list.finish())
        .with_padding(
            Padding::uniform(0.)
                .with_top(2.)
                .with_left(LIST_HORIZONTAL_PADDING)
                .with_right(LIST_HORIZONTAL_PADDING)
                .with_bottom(10.),
        )
        .finish()
}

/// The rows in the order the panel shows them: search-filtered, sorted, with
/// the freeze rule applied and (in group-by-project mode) grouped. Keyboard
/// navigation walks this, so it visits rows exactly as they are rendered.
/// The order is remembered like a render's, so that repeated navigation
/// steps while the order is frozen keep walking the same list.
pub(super) fn displayed_row_locators(
    state: &VerticalTabsPanelState,
    workspace: &Workspace,
    mode: VerticalTabsSortMode,
    app: &AppContext,
) -> Vec<PaneViewLocator> {
    let rows = collect_status_rows(state, workspace, app);
    let (sections, display_keys) = layout_sections(state, workspace, mode, &rows, app);
    store_last_order(state, display_keys);
    sections
        .into_iter()
        .flat_map(|section| section.row_indices)
        .filter_map(|row_index| rows.get(row_index))
        .map(|row| PaneViewLocator {
            pane_group_id: row.key.pane_group_id,
            pane_id: row.key.pane_id,
        })
        .collect()
}

/// Sorts the rows, applies the freeze rule, and splits them into sections.
/// Also returns the row order to remember for the freeze rule.
fn layout_sections(
    state: &VerticalTabsPanelState,
    workspace: &Workspace,
    mode: VerticalTabsSortMode,
    rows: &[StatusRow<'_>],
    app: &AppContext,
) -> (Vec<ListSection>, Vec<StatusRowKey>) {
    let sort_inputs: Vec<StatusSortInput<'_>> = rows.iter().map(StatusRow::sort_input).collect();
    let fresh_keys: Vec<StatusRowKey> = status_sorted_indices(&sort_inputs)
        .into_iter()
        .filter_map(|index| sort_inputs.get(index).map(|row| row.key))
        .collect();

    let frozen = is_status_list_frozen(state, workspace, app);
    let previous = last_order(state);
    let display_keys = resolve_display_order(&previous, &fresh_keys, frozen);
    let index_by_key: HashMap<StatusRowKey, usize> = sort_inputs
        .iter()
        .enumerate()
        .map(|(index, row)| (row.key, index))
        .collect();
    let display_order: Vec<usize> = display_keys
        .iter()
        .filter_map(|key| index_by_key.get(key).copied())
        .collect();

    let sections = match mode {
        VerticalTabsSortMode::GroupByProject => group_by_project(&sort_inputs, &display_order)
            .into_iter()
            .map(|group| ListSection {
                header: Some(SectionHeader {
                    text: group_header_text(&group),
                }),
                row_indices: group.row_indices,
            })
            .collect(),
        VerticalTabsSortMode::Status | VerticalTabsSortMode::Manual => vec![ListSection {
            header: None,
            row_indices: display_order,
        }],
    };
    (sections, display_keys)
}

/// Whether the row order is frozen right now; see [`is_order_frozen`].
fn is_status_list_frozen(
    state: &VerticalTabsPanelState,
    workspace: &Workspace,
    app: &AppContext,
) -> bool {
    let now = Instant::now();
    // Each guard is dropped before the next lock is taken, and before the
    // Hoverables for these handles are built.
    let is_mouse_over = |mouse_state: &MouseStateHandle| {
        mouse_state
            .lock()
            .map(|mouse_state| mouse_state.is_mouse_over_element())
            .unwrap_or(false)
    };
    let keyboard_step_at = state.status_list_keyboard_step_at.get();
    is_order_frozen(&FreezeInputs {
        // A closed panel can't be pointed at, even if it closed under the mouse.
        is_panel_open: workspace.vertical_tabs_panel_open,
        is_window_active: app.windows().active_window() == Some(workspace.window_id),
        is_pointer_over_list: is_mouse_over(&state.status_list_hover_state),
        is_pointer_over_detail_sidecar: is_mouse_over(&state.detail_sidecar_mouse_state),
        is_detail_sidecar_shown: state.has_active_detail_target(),
        since_real_pointer_move: state.status_list_pointer.since_last_real_move(now),
        since_keyboard_step: keyboard_step_at.map(|step_at| now.saturating_duration_since(step_at)),
        has_pointer_moved_since_keyboard_step: keyboard_step_at
            .is_some_and(|step_at| state.status_list_pointer.moved_after(step_at)),
        is_context_menu_open: workspace.show_tab_right_click_menu.is_some(),
        is_dragging_pane: any_workspace_pane_being_dragged(workspace, app),
    })
}

/// Extra time after a pointer idle limit ([`POINTER_IDLE_RELEASE`] or
/// [`POINTER_IDLE_CAP`]) before the re-render that lets an idle pointer's
/// freeze go. It also limits how often pointer moves reschedule that
/// re-render.
const POINTER_IDLE_RELEASE_SLACK: Duration = Duration::from_secs(1);

/// Real (not synthetic) pointer moves over the status list and its detail
/// sidecar. Hover state alone can't tell whether the pointer is still there,
/// so the freeze also needs a recent real move.
#[derive(Default)]
pub(super) struct StatusListPointerActivity {
    last_real_move_at: Cell<Option<Instant>>,
    /// The pending re-render that releases the freeze once the pointer has
    /// been idle for [`POINTER_IDLE_RELEASE`] (the limit while the window is
    /// inactive), and when it fires.
    idle_release_notify: Cell<Option<(TaskId, Instant)>>,
    /// The same for [`POINTER_IDLE_CAP`], the active window's limit.
    idle_cap_notify: Cell<Option<(TaskId, Instant)>>,
}

impl StatusListPointerActivity {
    fn since_last_real_move(&self, now: Instant) -> Option<Duration> {
        self.last_real_move_at
            .get()
            .map(|moved_at| now.saturating_duration_since(moved_at))
    }

    fn moved_after(&self, instant: Instant) -> bool {
        self.last_real_move_at
            .get()
            .is_some_and(|moved_at| moved_at > instant)
    }

    fn record_real_move(&self, ctx: &mut EventContext) {
        let now = Instant::now();
        self.last_real_move_at.set(Some(now));
        // Either limit can apply by the time it runs out, since the window
        // can become active or inactive in the meantime.
        schedule_idle_release(&self.idle_release_notify, now, POINTER_IDLE_RELEASE, ctx);
        schedule_idle_release(&self.idle_cap_notify, now, POINTER_IDLE_CAP, ctx);
    }
}

/// Keeps one re-render scheduled for when a pointer move at `moved_at` stops
/// freezing the list under `idle_limit`, so an idle pointer never keeps it
/// frozen.
fn schedule_idle_release(
    pending: &Cell<Option<(TaskId, Instant)>>,
    moved_at: Instant,
    idle_limit: Duration,
    ctx: &mut EventContext,
) {
    let Some(release_at) = moved_at.checked_add(idle_limit) else {
        return;
    };
    let is_scheduled = pending
        .get()
        .is_some_and(|(_, notify_at)| notify_at >= release_at);
    if is_scheduled {
        return;
    }
    if let Some((timer_id, _)) = pending.take() {
        ctx.clear_notify_timer(timer_id);
    }
    pending.set(Some(ctx.notify_after(
        idle_limit.saturating_add(POINTER_IDLE_RELEASE_SLACK),
    )));
}

/// Records real pointer moves over `child` (the status list or the detail
/// sidecar) without otherwise handling them.
pub(super) fn track_real_pointer_moves(
    child: Box<dyn Element>,
    activity: Rc<StatusListPointerActivity>,
) -> Box<dyn Element> {
    EventHandler::new(child)
        .with_always_handle()
        .on_mouse_in(
            move |ctx, _, _| {
                activity.record_real_move(ctx);
                DispatchEventResult::PropagateToParent
            },
            Some(MouseInBehavior {
                fire_on_synthetic_events: false,
                fire_when_covered: true,
            }),
        )
        .finish()
}

/// Whether any terminal row of the workspace shows an age label.
pub(super) fn any_row_shows_age(workspace: &Workspace, app: &AppContext) -> bool {
    let notifications = AgentNotificationsModel::as_ref(app);
    workspace.tabs.iter().any(|tab| {
        let pane_group = tab.pane_group.as_ref(app);
        pane_group.visible_pane_ids().iter().any(|pane_id| {
            pane_group
                .terminal_view_from_pane_id(*pane_id, app)
                .is_some_and(|view| notifications.last_activity_unix_ms(view.id()).is_some())
        })
    })
}

fn last_order(state: &VerticalTabsPanelState) -> Vec<StatusRowKey> {
    state
        .status_list_last_order
        .try_borrow()
        .map(|order| order.to_vec())
        .unwrap_or_default()
}

fn store_last_order(state: &VerticalTabsPanelState, order: Vec<StatusRowKey>) {
    if let Ok(mut last_order) = state.status_list_last_order.try_borrow_mut() {
        *last_order = order;
    }
}

/// With a single visible pane the tab's custom title names the row; with
/// several, each pane keeps its own title.
fn tab_title_override(
    pane_group: &PaneGroup,
    visible_pane_count: usize,
    app: &AppContext,
) -> Option<String> {
    (visible_pane_count == 1)
        .then(|| pane_group.custom_title(app))
        .flatten()
}

fn terminal_view_handle(
    typed: &TypedPane<'_>,
    app: &AppContext,
) -> Option<ViewHandle<TerminalView>> {
    match typed {
        TypedPane::Terminal(terminal_pane) => Some(terminal_pane.terminal_view(app)),
        TypedPane::Code(_)
        | TypedPane::CodeDiff
        | TypedPane::File
        | TypedPane::Notebook { .. }
        | TypedPane::Workflow { .. }
        | TypedPane::Settings
        | TypedPane::EnvVarCollection
        | TypedPane::EnvironmentManagement
        | TypedPane::AIFact
        | TypedPane::AIDocument
        | TypedPane::ExecutionProfileEditor
        | TypedPane::Other => None,
    }
}

fn terminal_project_name(terminal_view: &TerminalView) -> Option<String> {
    project_name(
        terminal_view
            .current_repo_path()
            .map(|repo_path| repo_path.display_name()),
        terminal_view.pwd().as_deref(),
    )
}

fn row_matches_query(
    props: &PaneProps<'_>,
    project: Option<&str>,
    query_lower: &str,
    app: &AppContext,
) -> bool {
    pane_matches_query(props, query_lower, app)
        || project.is_some_and(|project| project.to_lowercase().contains(query_lower))
}

fn collect_status_rows<'a>(
    state: &VerticalTabsPanelState,
    workspace: &'a Workspace,
    app: &'a AppContext,
) -> Vec<StatusRow<'a>> {
    let appearance = Appearance::as_ref(app);
    let notifications = AgentNotificationsModel::as_ref(app);
    let cli_sessions = CLIAgentSessionsModel::as_ref(app);
    let now_unix_ms = chrono::Utc::now().timestamp_millis();
    let query_lower = (!state.search_query.is_empty()).then(|| state.search_query.to_lowercase());
    let is_tab_being_renamed = workspace.current_workspace_state.is_tab_being_renamed();

    let mut rows = Vec::new();
    for (tab_index, tab) in workspace.tabs.iter().enumerate() {
        let pane_group = tab.pane_group.as_ref(app);
        let pane_group_id = tab.pane_group.id();
        let visible_pane_ids = pane_group.visible_pane_ids();
        let focused_pane_id = pane_group.focused_pane_id(app);
        let is_active_tab = tab_index == workspace.active_tab_index
            && !workspace
                .current_workspace_state
                .is_agent_management_view_open;
        let is_in_multi_tab_selection = workspace.is_tab_in_multi_tab_selection(tab_index);
        let title_override = tab_title_override(pane_group, visible_pane_ids.len(), app);

        for (pane_index, &pane_id) in visible_pane_ids.iter().enumerate() {
            let locator = PaneViewLocator {
                pane_group_id,
                pane_id,
            };
            let is_pane_being_renamed = workspace
                .current_workspace_state
                .is_pane_being_renamed(locator);
            // The tab's rename editor shows on the row of its focused pane.
            let shows_tab_rename_editor =
                is_active_tab && is_tab_being_renamed && pane_id == focused_pane_id;
            let mouse_state = state
                .pane_row_mouse_states
                .borrow_mut()
                .entry(pane_id)
                .or_default()
                .clone();
            let Some(props) = PaneProps::new(
                pane_group,
                pane_id,
                pane_group_id,
                is_active_tab,
                tab.in_multi_selection,
                is_in_multi_tab_selection,
                PaneRowState {
                    mouse_state,
                    title_mouse_state: None,
                    pane_color: None,
                    badge_mouse_states: PaneRowBadgeMouseStates::default(),
                },
                state.detail_hover_state(workspace.window_id),
                VerticalTabsDisplayGranularity::Panes,
                true,
                title_override.clone(),
                None,
                Some(tab_index),
                shows_tab_rename_editor,
                shows_tab_rename_editor.then(|| workspace.tab_rename_editor.clone()),
                is_pane_being_renamed,
                is_pane_being_renamed.then(|| workspace.pane_rename_editor.clone()),
                false,
                false,
                None,
                app,
            ) else {
                continue;
            };

            let terminal_view_handle = terminal_view_handle(&props.typed, app);
            let terminal_view = terminal_view_handle
                .as_ref()
                .map(|handle| handle.as_ref(app));
            let project = terminal_view.and_then(terminal_project_name);
            let interrupted_record =
                terminal_view.and_then(TerminalView::restorable_cli_agent_session);
            let interrupted_title = interrupted_record.map(|record| {
                record
                    .display_title()
                    .unwrap_or_else(|| format!("{} session", record.agent.display_name()))
            });
            if let Some(query_lower) = query_lower.as_deref()
                && !row_matches_query(&props, project.as_deref(), query_lower, app)
                && !interrupted_title
                    .as_ref()
                    .is_some_and(|title| title.to_lowercase().contains(query_lower))
            {
                continue;
            }

            let terminal_view_id = terminal_view_handle.as_ref().map(|handle| handle.id());
            let icon =
                resolve_icon_with_status_variant(&props.typed, &props.title, appearance, app);
            // An interrupted session has no live agent yet, but it was one.
            let is_cli_agent = matches!(icon, IconWithStatusVariant::CLIAgent { .. })
                || interrupted_record.is_some();
            let status = icon_status(icon);
            // Only a CLI agent's own status can be the optimistic "in
            // progress" it is registered with before the first prompt. The
            // session is identified by the id it would be resumed with, which
            // (unlike the live id) ignores nested headless runs in the pane.
            let is_awaiting_first_prompt = is_cli_agent
                && terminal_view_id.is_some_and(|id| {
                    cli_sessions.session(id).is_some_and(|session| {
                        notifications.is_cli_session_awaiting_first_prompt(
                            id,
                            session.session_context.resume_session_id.as_deref(),
                        )
                    })
                });
            let status = row_display_status(status, is_awaiting_first_prompt);
            let is_unread =
                terminal_view_id.is_some_and(|id| notifications.is_terminal_view_unread(id));
            let age = terminal_view_id
                .and_then(|id| notifications.last_activity_unix_ms(id))
                .map(|changed_at| activity_age(now_unix_ms, changed_at));
            let is_errored = terminal_view
                .is_some_and(|view| view.current_state().state == TerminalViewState::Errored);
            let title = status_row_title(&props, terminal_view, interrupted_title, app);
            let interrupted = interrupted_record.map(|record| InterruptedSession {
                is_remote: record.is_remote(),
            });

            rows.push(StatusRow {
                key: StatusRowKey {
                    pane_group_id,
                    pane_id,
                },
                tab_index,
                bucket: row_attention_bucket(status.as_ref(), is_unread, interrupted.is_some()),
                status,
                terminal_view_id,
                is_unread,
                is_errored,
                interrupted,
                project,
                age,
                fallback_rank: (tab_index, pane_index),
                title,
                saves_tab_position: pane_id == focused_pane_id,
                props,
            });
        }
    }
    rows
}

/// The status an icon variant carries as its badge. The row shows it as its
/// own glyph at the end instead of an icon.
fn icon_status(variant: IconWithStatusVariant) -> Option<ConversationStatus> {
    match variant {
        IconWithStatusVariant::OzAgent { status, .. }
        | IconWithStatusVariant::CLIAgent { status, .. }
        | IconWithStatusVariant::CustomAvatar { status, .. } => status,
        IconWithStatusVariant::Neutral { .. } | IconWithStatusVariant::NeutralElement { .. } => {
            None
        }
    }
}

/// The row's title, using the same precedence as the manual list: an active
/// rename editor, then a custom pane or tab name, then (for an interrupted
/// agent session) the session's title, then the terminal's primary line
/// (agent title, running command, ...), then the pane's own title.
fn status_row_title(
    props: &PaneProps<'_>,
    terminal_view: Option<&TerminalView>,
    interrupted_title: Option<String>,
    app: &AppContext,
) -> StatusRowTitle {
    if props.is_tab_being_renamed
        && let Some(editor) = props.rename_editor.as_ref()
    {
        return StatusRowTitle::Editor(editor.clone());
    }
    if props.is_pane_being_renamed
        && let Some(editor) = props.pane_rename_editor.as_ref()
    {
        return StatusRowTitle::Editor(editor.clone());
    }
    if let Some(title) = props
        .custom_vertical_tabs_title
        .as_ref()
        .or(props.display_title_override.as_ref())
    {
        return StatusRowTitle::Text {
            text: title.clone(),
            font: TerminalPrimaryLineFont::Ui,
        };
    }
    if let Some(text) = interrupted_title {
        return StatusRowTitle::Text {
            text,
            font: TerminalPrimaryLineFont::Ui,
        };
    }
    let Some(terminal_view) = terminal_view else {
        return StatusRowTitle::Text {
            text: props.displayed_title().to_owned(),
            font: TerminalPrimaryLineFont::Ui,
        };
    };
    // Each of these reads takes the terminal model lock on its own; none of
    // them is nested inside another.
    let terminal_title = terminal_view.terminal_title_from_shell();
    let working_directory = resolved_terminal_working_directory(terminal_view, app)
        .unwrap_or_else(|| terminal_title.clone());
    let agent_text = terminal_agent_text(terminal_view, app);
    let (conversation_title, cli_agent_title) =
        preferred_agent_tab_titles(&agent_text, agent_tab_text_preference(app));
    match terminal_primary_line_data(
        terminal_view.is_long_running_and_user_controlled(),
        conversation_title,
        cli_agent_title,
        terminal_title.as_str(),
        working_directory.as_str(),
        terminal_title_fallback_font(&agent_text),
        terminal_view.last_completed_command_text(),
    ) {
        TerminalPrimaryLineData::StatusText { text } => StatusRowTitle::Text {
            text,
            font: TerminalPrimaryLineFont::Ui,
        },
        TerminalPrimaryLineData::Text { text, font } => StatusRowTitle::Text { text, font },
    }
}

/// Colors of the status list that depend on whether the theme is dark.
struct StatusPalette {
    text_main: ColorU,
    text_sub: ColorU,
    text_faint: ColorU,
    /// The running glyph: a little softer than the main text.
    running: ColorU,
    /// The needs-you glyph.
    needs_you: ColorU,
    /// "k needs you" in group headers (darker on light themes, to stay
    /// readable as text).
    needs_you_text: ColorU,
}

impl StatusPalette {
    fn new(theme: &WarpTheme) -> Self {
        let is_dark = theme.inferred_color_scheme() == ColorScheme::LightOnDark;
        let font_color = theme.font_color(theme.background()).into_solid();
        let (needs_you, needs_you_text) = if is_dark {
            (NEEDS_YOU_AMBER_DARK, NEEDS_YOU_AMBER_DARK)
        } else {
            (NEEDS_YOU_AMBER_LIGHT, NEEDS_YOU_TEXT_LIGHT)
        };
        Self {
            text_main: coloru_with_opacity(font_color, 90),
            text_sub: coloru_with_opacity(font_color, 60),
            text_faint: coloru_with_opacity(font_color, 40),
            running: coloru_with_opacity(font_color, 78),
            needs_you,
            needs_you_text,
        }
    }
}

/// How a status row's shell looks: the selected row is a raised card, other
/// rows only show a hover or multi-selection fill.
pub(super) struct StatusRowLook {
    pub(super) background: Option<WarpThemeFill>,
    pub(super) border: Option<WarpThemeFill>,
    pub(super) drop_shadow: Option<DropShadow>,
}

pub(super) fn status_row_look(
    is_selected: bool,
    is_in_multi_selection: bool,
    is_hovered: bool,
    is_being_dragged: bool,
    theme: &WarpTheme,
) -> StatusRowLook {
    if !is_selected {
        return StatusRowLook {
            background: pane_row_background(
                None,
                false,
                is_in_multi_selection,
                is_hovered,
                is_being_dragged,
                theme,
            ),
            border: None,
            drop_shadow: None,
        };
    }
    if theme.inferred_color_scheme() == ColorScheme::LightOnDark {
        // A lighter pane. No shadow: the renderer draws a shadow under the
        // whole rect, so it would darken this translucent card.
        StatusRowLook {
            background: Some(theme.foreground().with_opacity(11)),
            border: Some(theme.foreground().with_opacity(9)),
            drop_shadow: None,
        }
    } else {
        // An opaque card lifted by a soft shadow.
        StatusRowLook {
            background: Some(WarpThemeFill::Solid(theme.background().into_solid())),
            border: Some(theme.foreground().with_opacity(6)),
            drop_shadow: Some(DropShadow {
                color: ColorU::new(0, 0, 0, 26),
                offset: vec2f(0., 1.),
                blur_radius: 2.,
                spread_radius: 3.,
            }),
        }
    }
}

fn render_status_row(
    row: StatusRow<'_>,
    show_project_tag: bool,
    palette: &StatusPalette,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let StatusRow {
        tab_index,
        mut props,
        status,
        terminal_view_id,
        is_unread,
        is_errored,
        interrupted,
        project,
        age,
        title,
        saves_tab_position,
        ..
    } = row;

    let mut title_line = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.);
    if is_errored {
        title_line.add_child(
            ConstrainedBox::new(
                WarpIcon::AlertTriangle
                    .to_warpui_icon(WarpThemeFill::Solid(theme.ui_error_color()))
                    .finish(),
            )
            .with_width(ERROR_ICON_SIZE)
            .with_height(ERROR_ICON_SIZE)
            .finish(),
        );
    }
    title_line.add_child(
        Shrinkable::new(
            1.,
            render_row_title(title, is_unread, palette, appearance, app),
        )
        .finish(),
    );
    if row_shows_synced_inputs_indicator(&props, app) {
        title_line.add_child(render_synced_inputs_indicator());
    }

    let mut trailing = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.);
    if show_project_tag && let Some(project) = project.as_deref() {
        trailing.add_child(render_project_tag(project, appearance));
    }
    if let Some(interrupted) = interrupted {
        // Muted: the row already sorts with the sessions that need the user.
        let label = if interrupted.is_remote {
            "Interrupted (SSH)"
        } else {
            "Interrupted"
        };
        trailing.add_child(
            Text::new_inline(label, appearance.ui_font_family(), ROW_META_FONT_SIZE)
                .with_color(palette.text_sub)
                .finish(),
        );
    }
    if let Some(age) = age {
        // Right-aligned in a fixed slot, so the tags before it line up.
        trailing.add_child(
            ConstrainedBox::new(
                Align::new(
                    Text::new_inline(
                        compact_age_label(age),
                        appearance.ui_font_family(),
                        ROW_META_FONT_SIZE,
                    )
                    .with_color(palette.text_sub)
                    .finish(),
                )
                .right()
                .finish(),
            )
            .with_width(ROW_AGE_WIDTH)
            .with_height(STATUS_GLYPH_SIZE)
            .finish(),
        );
    }
    trailing.add_child(render_status_glyph(
        status.as_ref(),
        is_unread,
        palette,
        theme,
    ));

    let content = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.)
        .with_child(Expanded::new(1., title_line.finish()).finish())
        .with_child(trailing.finish())
        .finish();

    props.status_row_click = Some(StatusRowClick {
        mark_read_terminal_view_id: terminal_view_id.filter(|_| is_unread),
    });
    // With the 1px border the shell always reserves, rows are 30px tall and
    // inset 10px on the left and 9px on the right.
    let row = render_pane_row_element(
        props,
        Padding::uniform(6.).with_left(9.).with_right(8.),
        true,
        content,
        theme,
    );
    if saves_tab_position {
        SavePosition::new(row, &tab_position_id(tab_index)).finish()
    } else {
        row
    }
}

fn render_row_title(
    title: StatusRowTitle,
    is_unread: bool,
    palette: &StatusPalette,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    match title {
        StatusRowTitle::Editor(editor) => render_inline_tab_rename_editor(&editor, appearance, app),
        StatusRowTitle::Text { text, font } => {
            let font_family = match font {
                TerminalPrimaryLineFont::Ui => appearance.ui_font_family(),
                TerminalPrimaryLineFont::Monospace => appearance.monospace_font_family(),
            };
            let mut text = Text::new_inline(text, font_family, ROW_TITLE_FONT_SIZE)
                .with_clip(ClipConfig::ellipsis())
                .with_color(palette.text_main);
            if is_unread {
                text = text.with_style(Properties::default().weight(Weight::Semibold));
            }
            text.finish()
        }
    }
}

fn project_color(project: &str, theme: &WarpTheme) -> ColorU {
    project_palette_color(project)
        .to_ansi_color(&theme.terminal_colors().normal)
        .into()
}

fn render_project_tag(project: &str, appearance: &Appearance) -> Box<dyn Element> {
    let color = project_color(project, appearance.theme());
    ConstrainedBox::new(
        Container::new(
            Text::new_inline(
                project.to_owned(),
                appearance.ui_font_family(),
                PROJECT_TAG_FONT_SIZE,
            )
            .with_clip(ClipConfig::ellipsis())
            .with_color(color)
            .finish(),
        )
        .with_horizontal_padding(6.)
        .with_vertical_padding(1.)
        .with_background_color(coloru_with_opacity(color, 14))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(5.)))
        .finish(),
    )
    .with_max_width(PROJECT_TAG_MAX_WIDTH)
    .finish()
}

/// A glyph centered in the fixed-size status slot.
fn status_slot(child: Box<dyn Element>, child_size: f32) -> Box<dyn Element> {
    let inset = ((STATUS_GLYPH_SIZE - child_size) / 2.).max(0.);
    Container::new(
        ConstrainedBox::new(child)
            .with_width(child_size)
            .with_height(child_size)
            .finish(),
    )
    .with_uniform_padding(inset)
    .finish()
}

fn status_icon(icon: WarpIcon, color: ColorU, size: f32) -> Box<dyn Element> {
    status_slot(
        icon.to_warpui_icon(WarpThemeFill::Solid(color)).finish(),
        size,
    )
}

/// One of the fork's own status glyphs, drawn at the full slot size.
fn status_glyph(path: &'static str, color: ColorU) -> Box<dyn Element> {
    status_slot(Icon::new(path, color).finish(), STATUS_GLYPH_SIZE)
}

/// The status at the end of a row: ring = needs you, open arc = running,
/// filled check = done and unread, faint check = done and read.
fn render_status_glyph(
    status: Option<&ConversationStatus>,
    is_unread: bool,
    palette: &StatusPalette,
    theme: &WarpTheme,
) -> Box<dyn Element> {
    match status {
        None => ConstrainedBox::new(Empty::new().finish())
            .with_width(STATUS_GLYPH_SIZE)
            .with_height(STATUS_GLYPH_SIZE)
            .finish(),
        Some(ConversationStatus::Blocked { .. }) => {
            status_glyph(NEEDS_YOU_GLYPH_PATH, palette.needs_you)
        }
        // Static on purpose: an animated spinner would redraw the window
        // continuously while any agent runs.
        Some(ConversationStatus::InProgress) => status_glyph(RUNNING_GLYPH_PATH, palette.running),
        Some(
            status @ (ConversationStatus::TransientError
            | ConversationStatus::WaitingForEvents
            | ConversationStatus::Cancelled),
        ) => {
            let (icon, color) = status.status_icon_and_color(theme, StatusColorStyle::Standard);
            status_icon(icon, color, 13.)
        }
        Some(ConversationStatus::Success) => {
            if is_unread {
                status_glyph(DONE_UNREAD_GLYPH_PATH, palette.text_main)
            } else {
                status_glyph(DONE_READ_GLYPH_PATH, palette.text_faint)
            }
        }
        Some(ConversationStatus::Error) => {
            let red = theme.ansi_fg_red();
            let color = if is_unread {
                red
            } else {
                coloru_with_opacity(red, 60)
            };
            status_icon(WarpIcon::Triangle, color, 13.)
        }
    }
}

/// "project  N ............ k needs you"
fn render_group_header(
    header: &SectionHeader,
    is_first: bool,
    palette: &StatusPalette,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let font_family = appearance.ui_font_family();
    let medium = Properties::default().weight(Weight::Medium);

    let name_and_count = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            Shrinkable::new(
                1.,
                Text::new_inline(
                    header.text.name.clone(),
                    font_family,
                    GROUP_HEADER_FONT_SIZE,
                )
                .with_clip(ClipConfig::ellipsis())
                .with_color(palette.text_sub)
                .with_style(medium)
                .finish(),
            )
            .finish(),
        )
        .with_child(
            Text::new_inline(
                header.text.count.clone(),
                font_family,
                GROUP_HEADER_FONT_SIZE,
            )
            .with_color(palette.text_faint)
            .finish(),
        );

    let mut row = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.)
        .with_child(Expanded::new(1., name_and_count.finish()).finish());
    if let Some(needs_you) = header.text.needs_you.as_ref() {
        row.add_child(
            Text::new_inline(needs_you.clone(), font_family, GROUP_HEADER_FONT_SIZE)
                .with_color(palette.needs_you_text)
                .with_style(medium)
                .finish(),
        );
    }

    Container::new(row.finish())
        .with_padding(
            Padding::uniform(0.)
                .with_top(if is_first { 10. } else { 16. })
                .with_bottom(6.)
                .with_left(10.)
                .with_right(10.),
        )
        .finish()
}

/// The "Order" section of the vertical tabs settings popup.
pub(super) fn render_sort_mode_section(
    state: &VerticalTabsPanelState,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let current = *TabSettings::as_ref(app).vertical_tabs_sort_mode.value();

    let mut section = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(
            Container::new(
                Text::new_inline("Order", appearance.ui_font_family(), 12.)
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
            )
            .with_horizontal_padding(16.)
            .with_margin_bottom(4.)
            .finish(),
        );
    for (label, mode, mouse_state) in [
        (
            "Status",
            VerticalTabsSortMode::Status,
            &state.status_sort_option_mouse_state,
        ),
        (
            "Group by project",
            VerticalTabsSortMode::GroupByProject,
            &state.group_by_project_sort_option_mouse_state,
        ),
        (
            "Manual",
            VerticalTabsSortMode::Manual,
            &state.manual_sort_option_mouse_state,
        ),
    ] {
        section.add_child(render_sort_mode_option(
            label,
            current == mode,
            mouse_state.clone(),
            mode,
            appearance,
            theme,
        ));
    }
    section.finish()
}

fn render_sort_mode_option(
    label: &'static str,
    is_selected: bool,
    mouse_state: MouseStateHandle,
    value: VerticalTabsSortMode,
    appearance: &Appearance,
    theme: &WarpTheme,
) -> Box<dyn Element> {
    const ICON_SIZE: f32 = 16.;
    const FONT_SIZE: f32 = 12.;
    const GAP: f32 = 8.;

    let main_text = theme.main_text_color(theme.background());
    Hoverable::new(mouse_state, move |hover_state| {
        let check_icon: Box<dyn Element> = if is_selected {
            ConstrainedBox::new(WarpIcon::Check.to_warpui_icon(main_text).finish())
                .with_width(ICON_SIZE)
                .with_height(ICON_SIZE)
                .finish()
        } else {
            ConstrainedBox::new(Empty::new().finish())
                .with_width(ICON_SIZE)
                .with_height(ICON_SIZE)
                .finish()
        };
        let row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(GAP)
            .with_child(check_icon)
            .with_child(
                Text::new_inline(label, appearance.ui_font_family(), FONT_SIZE)
                    .with_color(main_text.into())
                    .finish(),
            )
            .finish();
        let mut container = Container::new(row)
            .with_horizontal_padding(16.)
            .with_vertical_padding(2.);
        if hover_state.is_hovered() {
            container = container.with_background(internal_colors::fg_overlay_1(theme));
        }
        container.finish()
    })
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(WorkspaceAction::SetVerticalTabsSortMode(value));
    })
    .with_cursor(Cursor::PointingHand)
    .finish()
}
