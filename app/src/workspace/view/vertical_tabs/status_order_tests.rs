use std::time::Duration;

use warpui::EntityId;

use super::{
    AttentionBucket, GroupHeaderText, OTHER_PROJECT_LABEL, PROJECT_PALETTE, ProjectGroup,
    StatusRowKey, StatusSortInput, attention_bucket, compact_age_label, group_by_project,
    group_header_text, project_name, project_palette_color, project_palette_index,
    reconcile_frozen_order, resolve_display_order, row_display_status, status_sorted_indices,
};
use crate::ai::agent::conversation::ConversationStatus;
use crate::pane_group::{PaneId, TerminalPaneId};

fn pane_id() -> PaneId {
    TerminalPaneId::dummy_terminal_pane_id().into()
}

fn key(pane_group: usize) -> StatusRowKey {
    StatusRowKey {
        pane_group_id: EntityId::from_usize(pane_group),
        pane_id: pane_id(),
    }
}

/// A row with the given key and bucket; tests override other fields with
/// struct-update syntax.
fn row(key: StatusRowKey, bucket: AttentionBucket) -> StatusSortInput<'static> {
    StatusSortInput {
        key,
        bucket,
        is_unread: false,
        age: None,
        fallback_rank: (0, 0),
        project: None,
    }
}

fn secs(secs: u64) -> Option<Duration> {
    Some(Duration::from_secs(secs))
}

fn blocked() -> ConversationStatus {
    ConversationStatus::Blocked {
        blocked_action: "run rm -rf build".to_owned(),
    }
}

#[test]
fn attention_bucket_maps_every_status() {
    let cases = [
        (Some(blocked()), false, AttentionBucket::NeedsYou),
        (Some(blocked()), true, AttentionBucket::NeedsYou),
        (
            Some(ConversationStatus::InProgress),
            false,
            AttentionBucket::Running,
        ),
        (
            Some(ConversationStatus::InProgress),
            true,
            AttentionBucket::Running,
        ),
        (
            Some(ConversationStatus::TransientError),
            false,
            AttentionBucket::Running,
        ),
        (
            Some(ConversationStatus::TransientError),
            true,
            AttentionBucket::Running,
        ),
        (
            Some(ConversationStatus::WaitingForEvents),
            false,
            AttentionBucket::Running,
        ),
        (
            Some(ConversationStatus::WaitingForEvents),
            true,
            AttentionBucket::Running,
        ),
        (
            Some(ConversationStatus::Success),
            false,
            AttentionBucket::Idle,
        ),
        (
            Some(ConversationStatus::Success),
            true,
            AttentionBucket::Unread,
        ),
        (
            Some(ConversationStatus::Error),
            false,
            AttentionBucket::Idle,
        ),
        (
            Some(ConversationStatus::Error),
            true,
            AttentionBucket::Unread,
        ),
        (
            Some(ConversationStatus::Cancelled),
            false,
            AttentionBucket::Idle,
        ),
        (
            Some(ConversationStatus::Cancelled),
            true,
            AttentionBucket::Unread,
        ),
        (None, false, AttentionBucket::Idle),
        (None, true, AttentionBucket::Unread),
    ];
    for (status, is_unread, expected) in cases {
        assert_eq!(
            attention_bucket(status.as_ref(), is_unread),
            expected,
            "status {status:?}, unread {is_unread}"
        );
    }
}

#[test]
fn session_awaiting_first_prompt_is_idle_not_running() {
    // A freshly started CLI agent session reports InProgress before any prompt.
    let status = row_display_status(Some(ConversationStatus::InProgress), true);
    assert_eq!(status, None);
    assert_eq!(
        attention_bucket(status.as_ref(), false),
        AttentionBucket::Idle
    );
    assert_eq!(
        attention_bucket(status.as_ref(), true),
        AttentionBucket::Unread
    );

    // Once a prompt was submitted, InProgress means running.
    let status = row_display_status(Some(ConversationStatus::InProgress), false);
    assert_eq!(status, Some(ConversationStatus::InProgress));
    assert_eq!(
        attention_bucket(status.as_ref(), false),
        AttentionBucket::Running
    );
}

#[test]
fn awaiting_first_prompt_keeps_real_statuses() {
    for status in [
        ConversationStatus::Blocked {
            blocked_action: String::new(),
        },
        ConversationStatus::Success,
        ConversationStatus::Error,
        ConversationStatus::Cancelled,
        ConversationStatus::TransientError,
        ConversationStatus::WaitingForEvents,
    ] {
        assert_eq!(row_display_status(Some(status.clone()), true), Some(status));
    }
    assert_eq!(row_display_status(None, true), None);
    assert_eq!(row_display_status(None, false), None);
}

#[test]
fn bucket_ordering_matches_urgency() {
    assert!(AttentionBucket::NeedsYou < AttentionBucket::Unread);
    assert!(AttentionBucket::Unread < AttentionBucket::Running);
    assert!(AttentionBucket::Running < AttentionBucket::Idle);
}

#[test]
fn sort_puts_buckets_in_precedence_order() {
    let rows = vec![
        StatusSortInput {
            age: secs(1),
            ..row(key(1), AttentionBucket::Idle)
        },
        StatusSortInput {
            age: secs(1),
            ..row(key(2), AttentionBucket::Running)
        },
        StatusSortInput {
            is_unread: true,
            age: secs(1),
            ..row(key(3), AttentionBucket::Unread)
        },
        StatusSortInput {
            age: secs(500),
            ..row(key(4), AttentionBucket::NeedsYou)
        },
    ];
    assert_eq!(status_sorted_indices(&rows), vec![3, 2, 1, 0]);
}

#[test]
fn sort_puts_unread_first_within_bucket() {
    let rows = vec![
        StatusSortInput {
            age: secs(1),
            ..row(key(1), AttentionBucket::NeedsYou)
        },
        StatusSortInput {
            is_unread: true,
            age: secs(100),
            ..row(key(2), AttentionBucket::NeedsYou)
        },
        StatusSortInput {
            age: secs(1),
            ..row(key(3), AttentionBucket::Running)
        },
        StatusSortInput {
            is_unread: true,
            age: secs(100),
            ..row(key(4), AttentionBucket::Running)
        },
    ];
    assert_eq!(status_sorted_indices(&rows), vec![1, 0, 3, 2]);
}

#[test]
fn sort_is_newest_first_with_unknown_age_last() {
    let rows = vec![
        // No known activity, but the most recently used tab.
        row(key(1), AttentionBucket::Idle),
        StatusSortInput {
            age: secs(3_600),
            fallback_rank: (5, 0),
            ..row(key(2), AttentionBucket::Idle)
        },
        StatusSortInput {
            age: secs(10),
            fallback_rank: (6, 0),
            ..row(key(3), AttentionBucket::Idle)
        },
        StatusSortInput {
            age: secs(600),
            fallback_rank: (7, 0),
            ..row(key(4), AttentionBucket::Idle)
        },
    ];
    assert_eq!(status_sorted_indices(&rows), vec![2, 3, 1, 0]);
}

#[test]
fn sort_falls_back_to_rank_then_key() {
    let first = key(1);
    let second = key(2);
    let rows = vec![
        StatusSortInput {
            fallback_rank: (0, 1),
            ..row(second, AttentionBucket::Idle)
        },
        StatusSortInput {
            fallback_rank: (0, 1),
            ..row(first, AttentionBucket::Idle)
        },
        row(key(3), AttentionBucket::Idle),
        StatusSortInput {
            fallback_rank: (usize::MAX, 0),
            ..row(key(4), AttentionBucket::Idle)
        },
    ];
    // Rank (0, 0) first; the two (0, 1) rows tie and are ordered by key; the
    // unknown-MRU row is last.
    assert_eq!(status_sorted_indices(&rows), vec![2, 1, 0, 3]);
}

#[test]
fn sort_is_deterministic_regardless_of_input_order() {
    let rows = vec![
        row(key(1), AttentionBucket::Idle),
        StatusSortInput {
            age: secs(5),
            fallback_rank: (1, 0),
            ..row(key(2), AttentionBucket::Running)
        },
        StatusSortInput {
            age: secs(5),
            fallback_rank: (2, 0),
            ..row(key(3), AttentionBucket::NeedsYou)
        },
        StatusSortInput {
            age: secs(5),
            fallback_rank: (1, 1),
            ..row(key(4), AttentionBucket::Running)
        },
    ];
    let expected: Vec<StatusRowKey> = status_sorted_indices(&rows)
        .into_iter()
        .map(|index| rows[index].key)
        .collect();

    let mut reversed = rows.clone();
    reversed.reverse();
    let actual: Vec<StatusRowKey> = status_sorted_indices(&reversed)
        .into_iter()
        .map(|index| reversed[index].key)
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn sort_handles_empty_input() {
    assert!(status_sorted_indices(&[]).is_empty());
}

#[test]
fn group_by_project_orders_groups_by_first_appearance_and_counts_needs_you() {
    let rows = vec![
        StatusSortInput {
            project: Some("warp"),
            ..row(key(1), AttentionBucket::Idle)
        },
        StatusSortInput {
            age: secs(1),
            project: Some("api"),
            ..row(key(2), AttentionBucket::NeedsYou)
        },
        StatusSortInput {
            age: secs(2),
            project: Some("warp"),
            ..row(key(3), AttentionBucket::NeedsYou)
        },
        StatusSortInput {
            age: secs(3),
            ..row(key(4), AttentionBucket::Running)
        },
        StatusSortInput {
            age: secs(4),
            project: Some("api"),
            ..row(key(5), AttentionBucket::NeedsYou)
        },
    ];
    let order = status_sorted_indices(&rows);
    assert_eq!(order, vec![1, 2, 4, 3, 0]);

    let groups = group_by_project(&rows, &order);
    assert_eq!(
        groups,
        vec![
            ProjectGroup {
                project: Some("api"),
                row_indices: vec![1, 4],
                needs_you_count: 2,
            },
            ProjectGroup {
                project: Some("warp"),
                row_indices: vec![2, 0],
                needs_you_count: 1,
            },
            ProjectGroup {
                project: None,
                row_indices: vec![3],
                needs_you_count: 0,
            },
        ]
    );
}

#[test]
fn group_by_project_skips_out_of_range_indices() {
    let rows = vec![StatusSortInput {
        project: Some("warp"),
        ..row(key(1), AttentionBucket::Idle)
    }];
    let groups = group_by_project(&rows, &[5, 0, 7]);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].row_indices, vec![0]);
    assert!(group_by_project(&rows, &[]).is_empty());
}

#[test]
fn group_header_text_omits_needs_you_when_zero() {
    let quiet = ProjectGroup {
        project: Some("warp"),
        row_indices: vec![0, 1, 2],
        needs_you_count: 0,
    };
    assert_eq!(
        group_header_text(&quiet),
        GroupHeaderText {
            name: "warp".to_owned(),
            count: "· 3".to_owned(),
            needs_you: None,
        }
    );

    let urgent = ProjectGroup {
        project: None,
        row_indices: vec![0, 1, 2, 3],
        needs_you_count: 1,
    };
    assert_eq!(
        group_header_text(&urgent),
        GroupHeaderText {
            name: OTHER_PROJECT_LABEL.to_owned(),
            count: "· 4".to_owned(),
            needs_you: Some("· 1 needs you".to_owned()),
        }
    );
}

#[test]
fn reconcile_keeps_previous_positions_drops_removed_and_appends_new() {
    assert_eq!(
        reconcile_frozen_order(&[1, 2, 3, 4], &[4, 5, 2, 1]),
        vec![1, 2, 4, 5]
    );
}

#[test]
fn reconcile_handles_empty_inputs() {
    assert_eq!(reconcile_frozen_order::<u32>(&[], &[]), Vec::<u32>::new());
    assert_eq!(reconcile_frozen_order(&[], &[3, 1]), vec![3, 1]);
    assert_eq!(reconcile_frozen_order(&[3, 1], &[]), Vec::<i32>::new());
}

#[test]
fn reconcile_ignores_duplicates() {
    assert_eq!(reconcile_frozen_order(&[1, 1, 2], &[2, 1, 2]), vec![1, 2]);
}

#[test]
fn resolve_display_order_only_freezes_when_requested() {
    assert_eq!(
        resolve_display_order(&[1, 2], &[2, 1, 3], false),
        vec![2, 1, 3]
    );
    assert_eq!(
        resolve_display_order(&[1, 2], &[2, 1, 3], true),
        vec![1, 2, 3]
    );
    // Nothing to keep yet: use the fresh order even when frozen.
    assert_eq!(resolve_display_order(&[], &[2, 1], true), vec![2, 1]);
}

#[test]
fn palette_index_is_stable_and_in_range() {
    for project in ["", "warp", "api", "Launch", "a/b", "日本語", "warp"] {
        let index = project_palette_index(project);
        assert!(index < PROJECT_PALETTE.len(), "{project}: {index}");
        assert_eq!(index, project_palette_index(project));
        assert_eq!(
            project_palette_color(project),
            PROJECT_PALETTE[index],
            "{project}"
        );
    }
}

#[test]
fn palette_spreads_distinct_projects() {
    let names = [
        "warp", "api", "web", "infra", "docs", "mobile", "cli", "sdk",
    ];
    let distinct: std::collections::HashSet<usize> =
        names.into_iter().map(project_palette_index).collect();
    assert!(distinct.len() > 1);
}

#[test]
fn compact_age_label_boundaries() {
    let label = |secs| compact_age_label(Duration::from_secs(secs));
    assert_eq!(label(0), "now");
    assert_eq!(label(59), "now");
    assert_eq!(label(60), "1m");
    assert_eq!(label(240), "4m");
    assert_eq!(label(3_599), "59m");
    assert_eq!(label(3_600), "1h");
    assert_eq!(label(7_200), "2h");
    assert_eq!(label(86_399), "23h");
    assert_eq!(label(86_400), "1d");
    assert_eq!(label(3 * 86_400 + 5), "3d");
    assert_eq!(
        compact_age_label(Duration::MAX),
        format!("{}d", u64::MAX / 86_400)
    );
    assert_eq!(compact_age_label(Duration::from_millis(59_999)), "now");
}

#[test]
fn project_name_prefers_repo_root() {
    assert_eq!(
        project_name(Some("warp"), Some("/home/me/other")),
        Some("warp".to_owned())
    );
}

#[test]
fn project_name_falls_back_to_cwd_folder() {
    assert_eq!(
        project_name(None, Some("/home/me/api")),
        Some("api".to_owned())
    );
    assert_eq!(
        project_name(Some(""), Some("/home/me/api/")),
        Some("api".to_owned())
    );
    assert_eq!(
        project_name(Some("   "), Some("/home/me/api//")),
        Some("api".to_owned())
    );
    assert_eq!(
        project_name(None, Some("C:\\Users\\me\\proj\\")),
        Some("proj".to_owned())
    );
    assert_eq!(
        project_name(None, Some("relative")),
        Some("relative".to_owned())
    );
}

#[test]
fn project_name_handles_root_and_missing_paths() {
    assert_eq!(project_name(None, Some("/")), Some("/".to_owned()));
    assert_eq!(project_name(None, Some("///")), Some("/".to_owned()));
    assert_eq!(project_name(None, Some("")), None);
    assert_eq!(project_name(None, Some("  ")), None);
    assert_eq!(project_name(None, None), None);
    assert_eq!(project_name(Some(""), None), None);
}
