//! Pure ordering logic for the status-ordered vertical tabs sidebar.
//!
//! Nothing in this module touches `AppContext` or any model: callers gather
//! per-row inputs during render and hand them here, which keeps the sorting,
//! grouping and freeze rules unit-testable and free of locks or borrows.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::time::Duration;

use warp_core::ui::theme::AnsiColorIdentifier;
use warpui::EntityId;

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
    /// Tie-breaker for rows without a known age: (tab MRU position, pane position).
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
    let mut groups: Vec<ProjectGroup<'a>> = Vec::new();
    let mut group_index_by_project: HashMap<Option<&'a str>, usize> = HashMap::new();
    for &row_index in order {
        let Some(row) = rows.get(row_index) else {
            continue;
        };
        let group_index = *group_index_by_project
            .entry(row.project)
            .or_insert_with(|| {
                groups.push(ProjectGroup {
                    project: row.project,
                    row_indices: Vec::new(),
                    needs_you_count: 0,
                });
                groups.len().saturating_sub(1)
            });
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

/// The pieces of a group header: "proj", "· N", and optionally "· k needs you".
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GroupHeaderText {
    pub name: String,
    pub count: String,
    pub needs_you: Option<String>,
}

pub(super) fn group_header_text(group: &ProjectGroup<'_>) -> GroupHeaderText {
    GroupHeaderText {
        name: group.project.unwrap_or(OTHER_PROJECT_LABEL).to_owned(),
        count: format!("· {}", group.row_indices.len()),
        needs_you: (group.needs_you_count > 0)
            .then(|| format!("· {} needs you", group.needs_you_count)),
    }
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
/// order when the list is frozen (mouse over the list, menu open, dragging).
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
