//! Pure ordering logic for the status-ordered vertical tabs sidebar.
//!
//! Nothing in this module touches `AppContext` or any model: callers gather
//! per-row inputs during render and hand them here, which keeps the sorting,
//! grouping and freeze rules unit-testable and free of locks or borrows.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::time::Duration;

use pathfinder_color::ColorU;
use warpui::EntityId;
use warpui::elements::DEFAULT_UI_LINE_HEIGHT_RATIO;

use super::StatusRowNavigation;
use crate::ai::agent::conversation::ConversationStatus;
use crate::pane_group::PaneId;
use crate::workspace::project_labels::{
    ProjectLabelTone, VerticalTabsProjectColors, VerticalTabsProjectLabels,
};

/// How urgently a session wants the user's attention. Lower sorts first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum AttentionBucket {
    /// The agent is blocked on the user (permission, question, needs input).
    NeedsYou,
    /// The agent finished (successfully or not) and the user hasn't looked yet,
    /// or the user explicitly marked the session as unread.
    Unread,
    /// The agent is actively working (or waiting on events / retrying).
    Running,
    /// Everything else: read sessions and plain shells.
    Idle,
}

/// Maps a session's status and unread state to its attention bucket.
///
/// `NeedsYou` always wins. A running session stays `Running` even when it is
/// (manually) unread: the unread dot is still shown, but the session doesn't
/// jump above other running work until it actually finishes.
pub(super) fn attention_bucket(
    status: Option<&ConversationStatus>,
    is_unread: bool,
) -> AttentionBucket {
    let finished_or_none = || {
        if is_unread {
            AttentionBucket::Unread
        } else {
            AttentionBucket::Idle
        }
    };
    match status {
        Some(ConversationStatus::Blocked { .. }) => AttentionBucket::NeedsYou,
        Some(
            ConversationStatus::InProgress
            | ConversationStatus::TransientError
            | ConversationStatus::WaitingForEvents,
        ) => AttentionBucket::Running,
        Some(
            ConversationStatus::Success | ConversationStatus::Error | ConversationStatus::Cancelled,
        )
        | None => finished_or_none(),
    }
}

/// The bucket of a row. An interrupted agent session (its agent died in a
/// crash or restart and hasn't been resumed) needs the user to resume it, so
/// it needs them whatever the pane is doing now.
pub(super) fn row_attention_bucket(
    status: Option<&ConversationStatus>,
    is_unread: bool,
    is_interrupted: bool,
) -> AttentionBucket {
    if is_interrupted {
        AttentionBucket::NeedsYou
    } else {
        attention_bucket(status, is_unread)
    }
}

/// The status a row shows and sorts by. A CLI agent session is registered as
/// in progress before the user's first prompt, so until a prompt is
/// submitted that status means "started", not "working": the row is shown as
/// idle instead. Other statuses (blocked, finished, ...) are real events and
/// are kept.
pub(super) fn row_display_status(
    status: Option<ConversationStatus>,
    is_awaiting_first_prompt: bool,
) -> Option<ConversationStatus> {
    match status {
        Some(ConversationStatus::InProgress) if is_awaiting_first_prompt => None,
        status => status,
    }
}

/// Stable identity of a row across renders. Never keyed by tab index, since
/// tab indices shift when tabs are opened, closed or moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct StatusRowKey {
    pub pane_group_id: EntityId,
    pub pane_id: PaneId,
}

/// The per-row data the ordering needs.
#[derive(Clone, Debug)]
pub(super) struct StatusSortInput<'a> {
    pub key: StatusRowKey,
    pub bucket: AttentionBucket,
    pub is_unread: bool,
    /// Time since the session's last status change, if one has been observed.
    pub age: Option<Duration>,
    /// Tie-breaker for rows without a known age: (tab position, pane
    /// position). Stable on purpose: focusing a row never moves it, so
    /// stepping through rows can't bounce between two of them.
    pub fallback_rank: (usize, usize),
    pub project: Option<&'a str>,
}

fn compare_rows(a: &StatusSortInput<'_>, b: &StatusSortInput<'_>) -> Ordering {
    a.bucket
        .cmp(&b.bucket)
        // Unread first.
        .then_with(|| b.is_unread.cmp(&a.is_unread))
        // Newest status change first; rows without any known activity last.
        .then_with(|| match (a.age, b.age) {
            (Some(a_age), Some(b_age)) => a_age.cmp(&b_age),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| a.fallback_rank.cmp(&b.fallback_rank))
        .then_with(|| a.key.cmp(&b.key))
}

/// Returns indices into `rows` in display order: bucket, then unread, then
/// newest activity, then fallback rank, then key (fully deterministic).
pub(super) fn status_sorted_indices(rows: &[StatusSortInput<'_>]) -> Vec<usize> {
    let mut entries: Vec<(usize, &StatusSortInput<'_>)> = rows.iter().enumerate().collect();
    entries.sort_by(|(_, a), (_, b)| compare_rows(a, b));
    entries.into_iter().map(|(index, _)| index).collect()
}

/// A run of rows that belong to the same project, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectGroup<'a> {
    pub project: Option<&'a str>,
    /// Indices into the `rows` slice passed to [`group_by_project`].
    pub row_indices: Vec<usize>,
    pub needs_you_count: usize,
}

/// Groups `order` (indices into `rows`) by project. Groups appear in the order
/// of their first row, so when `order` is status-sorted the group containing
/// the most urgent session comes first. Out-of-range indices are skipped.
pub(super) fn group_by_project<'a>(
    rows: &[StatusSortInput<'a>],
    order: &[usize],
) -> Vec<ProjectGroup<'a>> {
    group_rows_by_project(rows, order, true)
}

/// The status order's grouping: like [`group_by_project`], except that rows
/// whose project is unknown are never grouped, so each is a group of its own.
/// The caller shows a header only for groups of two or more rows.
pub(super) fn auto_group_by_project<'a>(
    rows: &[StatusSortInput<'a>],
    order: &[usize],
) -> Vec<ProjectGroup<'a>> {
    group_rows_by_project(rows, order, false)
}

fn group_rows_by_project<'a>(
    rows: &[StatusSortInput<'a>],
    order: &[usize],
    group_unknown_projects: bool,
) -> Vec<ProjectGroup<'a>> {
    let mut groups: Vec<ProjectGroup<'a>> = Vec::new();
    let mut group_index_by_project: HashMap<Option<&'a str>, usize> = HashMap::new();
    for &row_index in order {
        let Some(row) = rows.get(row_index) else {
            continue;
        };
        let mut new_group = || {
            groups.push(ProjectGroup {
                project: row.project,
                row_indices: Vec::new(),
                needs_you_count: 0,
            });
            groups.len().saturating_sub(1)
        };
        let group_index = if row.project.is_none() && !group_unknown_projects {
            new_group()
        } else {
            *group_index_by_project
                .entry(row.project)
                .or_insert_with(new_group)
        };
        if let Some(group) = groups.get_mut(group_index) {
            group.row_indices.push(row_index);
            if row.bucket == AttentionBucket::NeedsYou {
                group.needs_you_count = group.needs_you_count.saturating_add(1);
            }
        }
    }
    groups
}

/// The per-row data the last-prompt order needs ([`last_prompt_sorted_indices`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct LastPromptSortInput {
    pub key: StatusRowKey,
    /// The session is waiting for the user's answer (see [`needs_answer`]).
    pub needs_answer: bool,
    /// Time since the session's last status change: for a session waiting
    /// for an answer, how long ago it asked.
    pub age: Option<Duration>,
    /// (tab position, pane position). The workspace keeps its tabs in the
    /// order they were last prompted, newest first.
    pub fallback_rank: (usize, usize),
}

/// Whether a row floats to the top of the last-prompt order: its agent is
/// blocked on the user (a permission or a question). An interrupted session
/// (its agent died in a crash or restart) waits for the user too, but stays
/// in place until it is resumed and prompted.
pub(super) fn needs_answer(status: Option<&ConversationStatus>, is_interrupted: bool) -> bool {
    !is_interrupted && matches!(status, Some(ConversationStatus::Blocked { .. }))
}

fn compare_last_prompt_rows(a: &LastPromptSortInput, b: &LastPromptSortInput) -> Ordering {
    // Rows waiting for an answer first.
    b.needs_answer
        .cmp(&a.needs_answer)
        // Among those, the newest request first; rows without a known age
        // last.
        .then_with(|| {
            if !(a.needs_answer && b.needs_answer) {
                return Ordering::Equal;
            }
            match (a.age, b.age) {
                (Some(a_age), Some(b_age)) => a_age.cmp(&b_age),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        })
        .then_with(|| a.fallback_rank.cmp(&b.fallback_rank))
        .then_with(|| a.key.cmp(&b.key))
}

/// Returns indices into `rows` in the last-prompt order: the rows waiting for
/// the user's answer, newest request first, then every other row in tab
/// order, which the workspace keeps sorted by when each tab was last
/// prompted. Ties fall back to the rank, then the key (fully deterministic).
pub(super) fn last_prompt_sorted_indices(rows: &[LastPromptSortInput]) -> Vec<usize> {
    let mut entries: Vec<(usize, &LastPromptSortInput)> = rows.iter().enumerate().collect();
    entries.sort_by(|(_, a), (_, b)| compare_last_prompt_rows(a, b));
    entries.into_iter().map(|(index, _)| index).collect()
}

/// The last-prompt order's grouping: consecutive rows (in `order`, indices
/// into `rows`) of the same project form a group, so a project shows as
/// several groups when rows of other projects sit between its rows. Rows
/// whose project is unknown are never grouped, so each is a group of its
/// own. The caller shows a header only for groups of two or more rows.
/// Out-of-range indices are skipped.
pub(super) fn group_adjacent_by_project<'a>(
    rows: &[StatusSortInput<'a>],
    order: &[usize],
) -> Vec<ProjectGroup<'a>> {
    let mut groups: Vec<ProjectGroup<'a>> = Vec::new();
    for &row_index in order {
        let Some(row) = rows.get(row_index) else {
            continue;
        };
        let joins_previous = row.project.is_some()
            && groups
                .last()
                .is_some_and(|group| group.project == row.project);
        if !joins_previous {
            groups.push(ProjectGroup {
                project: row.project,
                row_indices: Vec::new(),
                needs_you_count: 0,
            });
        }
        if let Some(group) = groups.last_mut() {
            group.row_indices.push(row_index);
            if row.bucket == AttentionBucket::NeedsYou {
                group.needs_you_count = group.needs_you_count.saturating_add(1);
            }
        }
    }
    groups
}

/// The most urgent of `row_indices` (indices into `rows`) by the status
/// order's rules, whose age and status a collapsed group header shows. `None`
/// when none of them is in range.
pub(super) fn most_urgent_row(
    rows: &[StatusSortInput<'_>],
    row_indices: &[usize],
) -> Option<usize> {
    row_indices
        .iter()
        .filter_map(|&index| rows.get(index).map(|row| (index, row)))
        .min_by(|(_, a), (_, b)| compare_rows(a, b))
        .map(|(index, _)| index)
}

/// Label used for rows whose project can't be determined.
pub(super) const OTHER_PROJECT_LABEL: &str = "Other";

/// The pieces of a group header: "proj", "N", and optionally "k needs you"
/// (shown at the far end of the header).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GroupHeaderText {
    pub name: String,
    pub count: String,
    pub needs_you: Option<String>,
}

pub(super) fn group_header_text(group: &ProjectGroup<'_>) -> GroupHeaderText {
    GroupHeaderText {
        name: group.project.unwrap_or(OTHER_PROJECT_LABEL).to_owned(),
        count: group.row_indices.len().to_string(),
        needs_you: (group.needs_you_count > 0)
            .then(|| format!("{} needs you", group.needs_you_count)),
    }
}

/// How a project's rows changed between two renders of the status order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum GroupTransition {
    /// A second session joined the project, so its badge became a group
    /// header.
    Formed,
    /// The project's group shrank to one session, so its header became that
    /// session's badge.
    Dissolved,
}

/// The project groups that formed or dissolved since the previous render, by
/// project name. `previous` holds the projects that had a group header then,
/// or is `None` when that render can't be compared (there was none yet, or
/// it used another order or a search), which never counts as a change.
/// `grouped` and `lone` hold the projects shown now with a header and as a
/// single badged row; a group whose rows all went away didn't dissolve.
pub(super) fn group_transitions(
    previous: Option<&HashSet<String>>,
    grouped: &HashSet<String>,
    lone: &HashSet<String>,
) -> Vec<(String, GroupTransition)> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let mut transitions: Vec<(String, GroupTransition)> = grouped
        .difference(previous)
        .map(|project| (project.clone(), GroupTransition::Formed))
        .chain(
            previous
                .difference(grouped)
                .filter(|project| lone.contains(*project))
                .map(|project| (project.clone(), GroupTransition::Dissolved)),
        )
        .collect();
    transitions.sort();
    transitions
}

/// How long a group header or badge slides into place when a group forms or
/// dissolves.
pub(super) const GROUP_TRANSITION_DURATION: Duration = Duration::from_millis(160);

/// How far along a slide lasting `duration` is after `elapsed`, eased out
/// (fast, then settling): 0 at the start, 1 once it's done.
pub(super) fn slide_progress(elapsed: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.;
    }
    let linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.);
    1. - (1. - linear).powi(3)
}

/// Keeps the previously displayed order while the list is frozen: rows that
/// still exist keep their previous position, rows that disappeared are
/// dropped, and new rows are appended in their fresh order.
pub(super) fn reconcile_frozen_order<K: Copy + Eq + Hash>(previous: &[K], fresh: &[K]) -> Vec<K> {
    let fresh_set: HashSet<K> = fresh.iter().copied().collect();
    let mut seen: HashSet<K> = HashSet::with_capacity(fresh.len());
    let mut result: Vec<K> = Vec::with_capacity(fresh.len());
    for key in previous {
        if fresh_set.contains(key) && seen.insert(*key) {
            result.push(*key);
        }
    }
    for key in fresh {
        if seen.insert(*key) {
            result.push(*key);
        }
    }
    result
}

/// Picks the order to display: the fresh order, or the reconciled previous
/// order when the list is frozen (see [`is_order_frozen`]).
pub(super) fn resolve_display_order<K: Copy + Eq + Hash>(
    previous: &[K],
    fresh: &[K],
    frozen: bool,
) -> Vec<K> {
    if frozen && !previous.is_empty() {
        reconcile_frozen_order(previous, fresh)
    } else {
        fresh.to_vec()
    }
}

/// How long the pointer may rest without really moving before it stops
/// freezing the order of a window that isn't the active (key) window. The UI
/// framework refreshes hover state with synthetic moves after every frame,
/// and an inactive window may never hear that the pointer left, so its hover
/// state alone could freeze the list forever.
pub(super) const POINTER_IDLE_RELEASE: Duration = Duration::from_secs(5);

/// The same limit for the active window, which does hear the pointer leave,
/// so a pointer resting on a row keeps it in place while the user reads. The
/// cap is only a last resort that keeps the list from freezing forever.
pub(super) const POINTER_IDLE_CAP: Duration = Duration::from_secs(120);

/// The longest keyboard navigation keeps the order frozen after its last
/// step, in case no other interaction ends it first.
pub(super) const KEYBOARD_NAVIGATION_FREEZE_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything the freeze rule depends on, gathered by the caller.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FreezeInputs {
    pub is_panel_open: bool,
    /// Whether this window is the active (key) window, which decides how long
    /// a resting pointer keeps the order frozen.
    pub is_window_active: bool,
    /// Whether the pointer was last over the row list.
    pub is_pointer_over_list: bool,
    /// Whether the pointer was last over the row detail sidecar.
    pub is_pointer_over_detail_sidecar: bool,
    /// Whether the detail sidecar is showing a row's details.
    pub is_detail_sidecar_shown: bool,
    /// Time since the pointer last really (not synthetically) moved over the
    /// list or the sidecar.
    pub since_real_pointer_move: Option<Duration>,
    /// Time since the last keyboard navigation step, unless another
    /// interaction (a click, typing, any other action) has ended it.
    pub since_keyboard_step: Option<Duration>,
    /// Whether the pointer really moved over the list or the sidecar after
    /// the last keyboard navigation step, which also ends it.
    pub has_pointer_moved_since_keyboard_step: bool,
    pub is_context_menu_open: bool,
    pub is_dragging_pane: bool,
}

/// Rows keep their positions while the user is pointing at the list or its
/// detail sidecar, is stepping through rows with the keyboard, has a context
/// menu open, or is dragging a pane, so nothing moves under them. Pointing
/// and keyboard navigation always run out on their own.
pub(super) fn is_order_frozen(inputs: &FreezeInputs) -> bool {
    let pointer_idle_limit = if inputs.is_window_active {
        POINTER_IDLE_CAP
    } else {
        POINTER_IDLE_RELEASE
    };
    let is_pointer_live = inputs.is_panel_open
        && inputs
            .since_real_pointer_move
            .is_some_and(|idle| idle < pointer_idle_limit);
    let is_pointing = is_pointer_live
        && (inputs.is_pointer_over_list
            || inputs.is_pointer_over_detail_sidecar
            || inputs.is_detail_sidecar_shown);
    let is_keyboard_navigating = !inputs.has_pointer_moved_since_keyboard_step
        && inputs
            .since_keyboard_step
            .is_some_and(|since| since < KEYBOARD_NAVIGATION_FREEZE_TIMEOUT);
    is_pointing || is_keyboard_navigating || inputs.is_context_menu_open || inputs.is_dragging_pane
}

/// The displayed row a navigation step lands on, as an index into
/// `row_count` rows. `current` is the position of the focused row, if it is
/// shown. Next and previous wrap around; a number past the last row lands
/// nowhere.
pub(super) fn navigation_target(
    navigation: StatusRowNavigation,
    current: Option<usize>,
    row_count: usize,
) -> Option<usize> {
    let last = row_count.checked_sub(1)?;
    let current = current.filter(|position| *position <= last);
    match navigation {
        StatusRowNavigation::Next => Some(match current {
            Some(position) if position < last => position + 1,
            _ => 0,
        }),
        StatusRowNavigation::Previous => Some(match current {
            Some(position) if position > 0 => position - 1,
            _ => last,
        }),
        StatusRowNavigation::Number(number) => number.checked_sub(1).filter(|index| *index <= last),
        StatusRowNavigation::Last => Some(last),
    }
}

/// The last non-empty component of a `/`- or `\`-separated path. The root
/// directory is reported as `/`.
fn last_path_component(path: &str) -> Option<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_trailing = trimmed.trim_end_matches(['/', '\\']);
    if without_trailing.is_empty() {
        // The path consisted only of separators, i.e. the root directory.
        return Some("/");
    }
    without_trailing
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
}

/// The project label for a session: the repository root's folder name when
/// known, otherwise the current directory's folder name.
pub(super) fn project_name(repo_root_name: Option<&str>, cwd: Option<&str>) -> Option<String> {
    repo_root_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .or_else(|| cwd.and_then(last_path_component))
        .map(str::to_owned)
}

/// Hues for project badges and group headers, as OKLCH hue angles in degrees
/// (rose, clay, amber, moss, sage, teal, slate, lavender), picked by a stable
/// hash of the project name unless the user picked one from the label's menu.
/// Every tone below has the same lightness and chroma in each hue, so no
/// project looks louder than another, and none depends on the theme's
/// terminal colors. These and the tones below are the built-in look; the
/// settings file can override each of them ([`VerticalTabsProjectLabels`]).
pub(super) const PROJECT_HUES: [f64; 8] = [12., 48., 82., 122., 158., 198., 256., 300.];

/// The names of [`PROJECT_HUES`], in the same order, as the settings file
/// saves them ([`VerticalTabsProjectColors`]).
pub(super) const PROJECT_COLOR_NAMES: [&str; 8] = [
    "rose", "clay", "amber", "moss", "sage", "teal", "slate", "lavender",
];

/// A tone of a project hue: OKLCH lightness (0 to 1) and chroma.
#[derive(Clone, Copy, Debug)]
struct Tone {
    lightness: f64,
    chroma: f64,
}

impl Tone {
    fn of(self, hue: f64) -> ColorU {
        oklch(self.lightness, self.chroma, hue)
    }
}

/// Text on light themes, and a group header's name.
const DEEP_TEXT: Tone = Tone {
    lightness: 0.45,
    chroma: 0.1,
};
/// Text on dark themes, and a group header's name.
const PALE_TEXT: Tone = Tone {
    lightness: 0.84,
    chroma: 0.08,
};
/// A label's fill on dark themes, top to bottom: a quiet tint that nearly
/// blends into the panel, so the color shows in the text.
const DARK_FILL: [Tone; 2] = [
    Tone {
        lightness: 0.37,
        chroma: 0.04,
    },
    Tone {
        lightness: 0.37,
        chroma: 0.04,
    },
];
/// A label's fill on light themes, top to bottom.
const LIGHT_FILL: [Tone; 2] = [
    Tone {
        lightness: 0.93,
        chroma: 0.03,
    },
    Tone {
        lightness: 0.93,
        chroma: 0.03,
    },
];
/// The glass edge's alpha on dark themes (white), top to bottom. Off unless
/// the settings file turns it on.
const DARK_RIM_ALPHA: [u8; 2] = [0, 0];
/// The glass edge on light themes: white on top, fading into the deep tone at
/// this alpha at the bottom. Off unless the settings file turns it on.
const LIGHT_RIM_TOP_ALPHA: u8 = 0;
const LIGHT_RIM_BOTTOM_ALPHA: u8 = 0;

/// The shadow's alpha on dark themes (black). Off unless the settings file
/// turns it on.
const DARK_SHADOW_ALPHA: u8 = 0;
const DARK_SHADOW_BLUR: f64 = 1.5;
/// The shadow on light themes: a cool near-black at this alpha. Off unless the
/// settings file turns it on.
const LIGHT_SHADOW_ALPHA: u8 = 0;
const LIGHT_SHADOW_BLUR: f64 = 1.;
/// Upper bounds for values from the settings file. OKLCH chroma beyond this
/// is outside sRGB for every hue.
const MAX_CHROMA: f64 = 0.4;
const MAX_SHADOW_BLUR: f64 = 8.;

/// A stable (FNV-1a) slot for `project` in a palette of `len` hues, always in
/// range (zero for an empty palette).
pub(super) fn project_palette_index(project: &str, len: usize) -> usize {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = project.bytes().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    });
    if len == 0 {
        return 0;
    }
    (hash % len as u64) as usize
}

/// The slot in [`PROJECT_HUES`] of a color name from the settings file,
/// ignoring case and surrounding spaces. `None` for any other name.
pub(super) fn project_color_index(name: &str) -> Option<usize> {
    let name = name.trim();
    PROJECT_COLOR_NAMES
        .iter()
        .position(|known| known.eq_ignore_ascii_case(name))
}

/// The slot in [`PROJECT_HUES`] of the color picked for `project` from its
/// label's menu. `None` when it has none, or the file names an unknown color.
pub(super) fn picked_project_color(
    project: &str,
    colors: &VerticalTabsProjectColors,
) -> Option<usize> {
    colors.color_name(project).and_then(project_color_index)
}

/// The slot in [`PROJECT_HUES`] whose hue is `hue`, if any: how the color menu
/// marks and names an automatic color.
pub(super) fn project_color_of_hue(hue: f64) -> Option<usize> {
    PROJECT_HUES
        .iter()
        .position(|known| (known - hue).abs() < 1e-9)
}

/// The hue of `project`'s badge and group header: the color picked from its
/// label's menu if it has one, else its automatic hue
/// ([`automatic_project_hue`]).
pub(super) fn project_hue(
    project: &str,
    labels: &VerticalTabsProjectLabels,
    colors: &VerticalTabsProjectColors,
) -> f64 {
    match picked_project_color(project, colors).and_then(|index| PROJECT_HUES.get(index)) {
        Some(hue) => *hue,
        None => automatic_project_hue(project, labels),
    }
}

/// The hue `project` gets without a picked color: its hue from the settings
/// file if it has one, else its slot in the settings file's palette, else its
/// slot in [`PROJECT_HUES`]. Non-finite hues are skipped, and every hue is
/// wrapped into 0 to 360.
pub(super) fn automatic_project_hue(project: &str, labels: &VerticalTabsProjectLabels) -> f64 {
    if let Some(hue) = labels
        .project_hues
        .get(project)
        .copied()
        .filter(|hue| hue.is_finite())
    {
        return hue.rem_euclid(360.);
    }
    let custom: Vec<f64> = labels
        .hues
        .iter()
        .flatten()
        .copied()
        .filter(|hue| hue.is_finite())
        .map(|hue| hue.rem_euclid(360.))
        .collect();
    let palette: &[f64] = if custom.is_empty() {
        &PROJECT_HUES
    } else {
        &custom
    };
    palette
        .get(project_palette_index(project, palette.len()))
        .copied()
        .unwrap_or(PROJECT_HUES[0])
}

/// `value` from the settings file clamped to `min..=max`, or `default` when it
/// is unset or not finite.
pub(super) fn bounded_setting(value: Option<f64>, default: f64, min: f64, max: f64) -> f64 {
    value
        .filter(|value| value.is_finite())
        .map_or(default, |value| value.clamp(min, max))
}

/// How far a lone row's badge sticks out above the row: half the badge's
/// height (its text line, padding and edge), so it straddles the row's top
/// edge.
pub(super) fn badge_overhang(font_size: f32, vertical_padding: f32, edge_width: f32) -> f32 {
    (font_size * DEFAULT_UI_LINE_HEIGHT_RATIO + 2. * (vertical_padding + edge_width)) / 2.
}

/// The dashes of a dashed line `length` long, as (offset, length) runs from
/// its start. Like a browser's dashed border, it starts and ends with a full
/// dash and evens out the gaps, keeping them as close to `gap` as it can. A
/// line too short for two dashes is one solid run, and an empty or broken
/// length has no dashes.
pub(super) fn dash_runs(length: f32, dash: f32, gap: f32) -> Vec<(f32, f32)> {
    if !length.is_finite() || length <= 0. {
        return Vec::new();
    }
    if !(dash > 0. && gap > 0.) || length < 2. * dash + gap {
        return vec![(0., length)];
    }
    let gap_for = |count: f32| (length - count * dash) / (count - 1.);
    let fewer = (length / (dash + gap)).floor().max(2.);
    let more = fewer + 1.;
    let (count, spacing) =
        if gap_for(more) > 0. && (gap_for(more) - gap).abs() < (gap_for(fewer) - gap).abs() {
            (more, gap_for(more))
        } else {
            (fewer, gap_for(fewer))
        };
    (0..count as usize)
        .map(|index| (index as f32 * (dash + spacing), dash))
        .collect()
}

/// An alpha from a 0-to-1 strength in the settings file, or `default`.
fn alpha_setting(value: Option<f64>, default: u8) -> u8 {
    value
        .filter(|value| value.is_finite())
        .map_or(default, |value| (value.clamp(0., 1.) * 255.).round() as u8)
}

/// `default` with whatever the settings file overrides of it.
fn tone_setting(value: Option<ProjectLabelTone>, default: Tone) -> Tone {
    let Some(value) = value else {
        return default;
    };
    Tone {
        lightness: bounded_setting(value.lightness, default.lightness, 0., 1.),
        chroma: bounded_setting(value.chroma, default.chroma, 0., MAX_CHROMA),
    }
}

/// How a project label (a lone row's badge, or a group header's name) is
/// drawn. Fills and rims are vertical gradients, top to bottom. The shadow
/// falls one pixel down, blurred by `shadow_blur` (a Gaussian sigma).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ProjectPillStyle {
    pub(super) fill_top: ColorU,
    pub(super) fill_bottom: ColorU,
    pub(super) rim_top: ColorU,
    pub(super) rim_bottom: ColorU,
    pub(super) text: ColorU,
    pub(super) shadow: ColorU,
    pub(super) shadow_blur: f32,
}

/// A flat label in a quiet tint of `hue`, with its text in a deep (light
/// themes) or bright (dark themes) shade of the same hue, which is also the
/// color of a group header's name. The settings file can add a glass edge (a
/// hairline that is bright on top and fades downward) and a soft shadow; both
/// are off by default. `labels` holds the settings file's overrides for the
/// theme's kind.
pub(super) fn project_pill_style(
    hue: f64,
    dark_theme: bool,
    labels: &VerticalTabsProjectLabels,
) -> ProjectPillStyle {
    if dark_theme {
        let tones = &labels.dark;
        ProjectPillStyle {
            fill_top: tone_setting(tones.fill_top, DARK_FILL[0]).of(hue),
            fill_bottom: tone_setting(tones.fill_bottom, DARK_FILL[1]).of(hue),
            rim_top: ColorU::new(
                255,
                255,
                255,
                alpha_setting(tones.edge_top, DARK_RIM_ALPHA[0]),
            ),
            rim_bottom: ColorU::new(
                255,
                255,
                255,
                alpha_setting(tones.edge_bottom, DARK_RIM_ALPHA[1]),
            ),
            text: tone_setting(tones.text, PALE_TEXT).of(hue),
            shadow: ColorU::new(0, 0, 0, alpha_setting(tones.shadow, DARK_SHADOW_ALPHA)),
            shadow_blur: bounded_setting(tones.shadow_blur, DARK_SHADOW_BLUR, 0., MAX_SHADOW_BLUR)
                as f32,
        }
    } else {
        let tones = &labels.light;
        let deep = tone_setting(tones.text, DEEP_TEXT).of(hue);
        ProjectPillStyle {
            fill_top: tone_setting(tones.fill_top, LIGHT_FILL[0]).of(hue),
            fill_bottom: tone_setting(tones.fill_bottom, LIGHT_FILL[1]).of(hue),
            rim_top: ColorU::new(
                255,
                255,
                255,
                alpha_setting(tones.edge_top, LIGHT_RIM_TOP_ALPHA),
            ),
            rim_bottom: ColorU {
                a: alpha_setting(tones.edge_bottom, LIGHT_RIM_BOTTOM_ALPHA),
                ..deep
            },
            text: deep,
            shadow: ColorU::new(31, 35, 48, alpha_setting(tones.shadow, LIGHT_SHADOW_ALPHA)),
            shadow_blur: bounded_setting(tones.shadow_blur, LIGHT_SHADOW_BLUR, 0., MAX_SHADOW_BLUR)
                as f32,
        }
    }
}

/// An opaque sRGB color from OKLCH (lightness 0 to 1, chroma, hue in degrees),
/// with each channel clipped to the sRGB gamut.
fn oklch(lightness: f64, chroma: f64, hue_degrees: f64) -> ColorU {
    let (sin, cos) = hue_degrees.to_radians().sin_cos();
    let (a, b) = (chroma * cos, chroma * sin);
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.291485548 * b).powi(3);
    let encode = |linear: f64| {
        let linear = linear.clamp(0., 1.);
        let encoded = if linear <= 0.0031308 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1. / 2.4) - 0.055
        };
        (encoded * 255.).round() as u8
    };
    ColorU::new(
        encode(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
        encode(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
        encode(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
        0xFF,
    )
}

/// How long before `now_unix_ms` the change at `changed_at_unix_ms` was; zero
/// if the clock went backwards.
pub(super) fn activity_age(now_unix_ms: i64, changed_at_unix_ms: i64) -> Duration {
    u64::try_from(now_unix_ms.saturating_sub(changed_at_unix_ms))
        .map_or(Duration::ZERO, Duration::from_millis)
}

/// A compact relative age: "now", "4m", "2h", "3d".
pub(super) fn compact_age_label(age: Duration) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let secs = age.as_secs();
    if secs < MINUTE {
        "now".to_owned()
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else {
        format!("{}d", secs / DAY)
    }
}

#[cfg(test)]
#[path = "status_order_tests.rs"]
mod tests;
