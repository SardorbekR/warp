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
use warp_core::ui::theme::AnsiColorIdentifier;
use warpui::EntityId;

use super::StatusRowNavigation;
use crate::ai::agent::conversation::ConversationStatus;
use crate::pane_group::PaneId;

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

/// Whether text drawn on `background` (a project badge or header) should be
/// dark rather than light, so it stays readable on light and dark project
/// colors alike.
pub(super) fn wants_dark_text(background: ColorU) -> bool {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.;
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * channel(background.r)
        + 0.7152 * channel(background.g)
        + 0.0722 * channel(background.b);
    // Where black and white text have the same contrast ratio.
    luminance > 0.179
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

/// Colors used for project tags, picked by a stable hash of the project name.
pub(super) const PROJECT_PALETTE: [AnsiColorIdentifier; 6] = [
    AnsiColorIdentifier::Red,
    AnsiColorIdentifier::Green,
    AnsiColorIdentifier::Yellow,
    AnsiColorIdentifier::Blue,
    AnsiColorIdentifier::Magenta,
    AnsiColorIdentifier::Cyan,
];

/// A stable (FNV-1a) palette index for `project`, always in range.
pub(super) fn project_palette_index(project: &str) -> usize {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = project.bytes().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    });
    let len = PROJECT_PALETTE.len() as u64;
    // `len` is a non-zero constant, and the remainder is always < 6.
    (hash % len) as usize
}

pub(super) fn project_palette_color(project: &str) -> AnsiColorIdentifier {
    PROJECT_PALETTE
        .get(project_palette_index(project))
        .copied()
        .unwrap_or(AnsiColorIdentifier::Blue)
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
