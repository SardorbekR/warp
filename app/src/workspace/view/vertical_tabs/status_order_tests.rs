use std::collections::HashSet;
use std::time::Duration;

use pathfinder_color::ColorU;
use warpui::EntityId;

use super::{
    AttentionBucket, FreezeInputs, GroupHeaderText, GroupTransition,
    KEYBOARD_NAVIGATION_FREEZE_TIMEOUT, OTHER_PROJECT_LABEL, POINTER_IDLE_CAP,
    POINTER_IDLE_RELEASE, PROJECT_PALETTE, ProjectGroup, StatusRowKey, StatusRowNavigation,
    StatusSortInput, activity_age, attention_bucket, auto_group_by_project, compact_age_label,
    group_by_project, group_header_text, group_transitions, is_order_frozen, navigation_target,
    project_name, project_palette_color, project_palette_index, reconcile_frozen_order,
    resolve_display_order, row_attention_bucket, row_display_status, slide_progress,
    status_sorted_indices, wants_dark_text,
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
fn auto_grouping_never_groups_rows_of_an_unknown_project() {
    let rows = vec![
        StatusSortInput {
            project: Some("warp"),
            ..row(key(1), AttentionBucket::NeedsYou)
        },
        row(key(2), AttentionBucket::Unread),
        StatusSortInput {
            project: Some("api"),
            ..row(key(3), AttentionBucket::Running)
        },
        StatusSortInput {
            project: Some("warp"),
            ..row(key(4), AttentionBucket::Idle)
        },
        row(key(5), AttentionBucket::Idle),
    ];
    let order = vec![0, 1, 2, 3, 4];
    let groups = auto_group_by_project(&rows, &order);
    assert_eq!(
        groups,
        vec![
            // The most urgent row's group comes first and pulls its later row
            // up with it.
            ProjectGroup {
                project: Some("warp"),
                row_indices: vec![0, 3],
                needs_you_count: 1,
            },
            ProjectGroup {
                project: None,
                row_indices: vec![1],
                needs_you_count: 0,
            },
            ProjectGroup {
                project: Some("api"),
                row_indices: vec![2],
                needs_you_count: 0,
            },
            ProjectGroup {
                project: None,
                row_indices: vec![4],
                needs_you_count: 0,
            },
        ]
    );
    // Grouping by project puts the unknown rows together, after the first
    // one's position.
    let by_project = group_by_project(&rows, &order);
    assert_eq!(by_project.len(), 3);
    assert_eq!(by_project[1].project, None);
    assert_eq!(by_project[1].row_indices, vec![1, 4]);
}

fn projects(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn group_transitions_report_groups_that_formed_or_dissolved() {
    let previous = projects(&["warp", "api"]);
    let transitions = group_transitions(
        Some(&previous),
        // "api" is still grouped, "docs" just formed.
        &projects(&["api", "docs"]),
        // "warp" is down to one session; "web" had no group before.
        &projects(&["warp", "web"]),
    );
    assert_eq!(
        transitions,
        vec![
            ("docs".to_owned(), GroupTransition::Formed),
            ("warp".to_owned(), GroupTransition::Dissolved),
        ]
    );
}

#[test]
fn a_group_whose_sessions_all_closed_did_not_dissolve() {
    let previous = projects(&["warp"]);
    assert!(group_transitions(Some(&previous), &projects(&[]), &projects(&[])).is_empty());
}

#[test]
fn an_incomparable_previous_frame_never_animates() {
    assert!(group_transitions(None, &projects(&["warp"]), &projects(&["api"])).is_empty());
}

#[test]
fn slides_ease_out_from_start_to_end() {
    let duration = Duration::from_millis(160);
    assert_eq!(slide_progress(Duration::ZERO, duration), 0.);
    assert_eq!(slide_progress(duration, duration), 1.);
    assert_eq!(slide_progress(Duration::from_secs(5), duration), 1.);
    let halfway = slide_progress(Duration::from_millis(80), duration);
    // Eased out: past the middle at half time.
    assert!(halfway > 0.5 && halfway < 1., "{halfway}");
    let mut last = 0.;
    for ms in 0..=160 {
        let progress = slide_progress(Duration::from_millis(ms), duration);
        assert!(progress >= last, "{ms} ms");
        last = progress;
    }
    assert_eq!(slide_progress(Duration::from_millis(3), Duration::ZERO), 1.);
}

#[test]
fn badge_text_contrasts_with_the_project_color() {
    let rgb = |r, g, b| ColorU::new(r, g, b, 0xFF);
    assert!(wants_dark_text(rgb(0xFF, 0xFF, 0xFF)));
    assert!(!wants_dark_text(rgb(0x00, 0x00, 0x00)));
    // Light theme colors (a pale yellow, a light green).
    assert!(wants_dark_text(rgb(0xFE, 0xFD, 0xC2)));
    assert!(wants_dark_text(rgb(0xB4, 0xFA, 0x72)));
    // Saturated dark ones (a deep blue, a dark red).
    assert!(!wants_dark_text(rgb(0x1E, 0x3A, 0x8A)));
    assert!(!wants_dark_text(rgb(0x99, 0x1B, 0x1B)));
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
            count: "3".to_owned(),
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
            count: "4".to_owned(),
            needs_you: Some("1 needs you".to_owned()),
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
fn activity_age_is_wall_clock_difference_and_never_negative() {
    assert_eq!(activity_age(10_000, 4_000), Duration::from_millis(6_000));
    assert_eq!(activity_age(10_000, 10_000), Duration::ZERO);
    // The clock went backwards (or a time from the future).
    assert_eq!(activity_age(4_000, 10_000), Duration::ZERO);
    assert_eq!(
        activity_age(i64::MAX, i64::MIN),
        Duration::from_millis(i64::MAX as u64)
    );
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

#[test]
fn interrupted_session_needs_you_whatever_its_status() {
    let statuses = [
        None,
        Some(blocked()),
        Some(ConversationStatus::InProgress),
        Some(ConversationStatus::WaitingForEvents),
        Some(ConversationStatus::TransientError),
        Some(ConversationStatus::Success),
        Some(ConversationStatus::Error),
        Some(ConversationStatus::Cancelled),
    ];
    for status in &statuses {
        for is_unread in [false, true] {
            assert_eq!(
                row_attention_bucket(status.as_ref(), is_unread, true),
                AttentionBucket::NeedsYou,
                "interrupted, status {status:?}, unread {is_unread}"
            );
            assert_eq!(
                row_attention_bucket(status.as_ref(), is_unread, false),
                attention_bucket(status.as_ref(), is_unread),
                "not interrupted, status {status:?}, unread {is_unread}"
            );
        }
    }
}

#[test]
fn interrupted_session_sorts_with_sessions_that_need_you() {
    let rows = [
        row(key(1), AttentionBucket::Running),
        row(
            key(2),
            row_attention_bucket(None, false, /* is_interrupted */ true),
        ),
        row(key(3), AttentionBucket::Unread),
        row(key(4), row_attention_bucket(None, false, false)),
    ];
    assert_eq!(status_sorted_indices(&rows), vec![1, 2, 0, 3]);
}

/// Freeze inputs for an open panel in the active window, with nothing
/// freezing the order.
fn idle_freeze_inputs() -> FreezeInputs {
    FreezeInputs {
        is_panel_open: true,
        is_window_active: true,
        ..FreezeInputs::default()
    }
}

fn recently() -> Option<Duration> {
    Some(Duration::from_millis(200))
}

#[test]
fn order_is_not_frozen_without_a_reason() {
    assert!(!is_order_frozen(&idle_freeze_inputs()));
    assert!(!is_order_frozen(&FreezeInputs::default()));
}

fn just_under(limit: Duration) -> Option<Duration> {
    limit.checked_sub(Duration::from_millis(1))
}

#[test]
fn pointer_resting_on_the_list_of_the_active_window_keeps_it_frozen() {
    let pointing = FreezeInputs {
        is_pointer_over_list: true,
        since_real_pointer_move: recently(),
        ..idle_freeze_inputs()
    };
    assert!(is_order_frozen(&pointing));

    // Hover state alone (e.g. refreshed by synthetic moves) doesn't freeze.
    assert!(!is_order_frozen(&FreezeInputs {
        since_real_pointer_move: None,
        ..pointing
    }));
    // The active window hears the pointer leave, so a pointer resting on a
    // row while the user reads the list keeps every row in place...
    for idle in [
        POINTER_IDLE_RELEASE,
        Duration::from_secs(6),
        Duration::from_secs(60),
    ] {
        assert!(
            is_order_frozen(&FreezeInputs {
                since_real_pointer_move: Some(idle),
                ..pointing
            }),
            "{idle:?}"
        );
    }
    assert!(is_order_frozen(&FreezeInputs {
        since_real_pointer_move: just_under(POINTER_IDLE_CAP),
        ..pointing
    }));
    // ...up to a last-resort cap, so the list can never stay frozen forever.
    assert!(!is_order_frozen(&FreezeInputs {
        since_real_pointer_move: Some(POINTER_IDLE_CAP),
        ..pointing
    }));
    // A closed panel can't be pointed at, even if it closed under the pointer.
    assert!(!is_order_frozen(&FreezeInputs {
        is_panel_open: false,
        ..pointing
    }));
    // Moving elsewhere doesn't freeze the list.
    assert!(!is_order_frozen(&FreezeInputs {
        is_pointer_over_list: false,
        ..pointing
    }));
}

#[test]
fn pointer_over_the_list_of_an_inactive_window_freezes_only_while_it_moves() {
    let pointing = FreezeInputs {
        is_window_active: false,
        is_pointer_over_list: true,
        since_real_pointer_move: recently(),
        ..idle_freeze_inputs()
    };
    assert!(is_order_frozen(&pointing));

    // An inactive window may never hear that the pointer left, so a resting
    // pointer releases it after a short idle time.
    assert!(is_order_frozen(&FreezeInputs {
        since_real_pointer_move: just_under(POINTER_IDLE_RELEASE),
        ..pointing
    }));
    assert!(!is_order_frozen(&FreezeInputs {
        since_real_pointer_move: Some(POINTER_IDLE_RELEASE),
        ..pointing
    }));
    // So losing focus after the pointer has rested a while releases the list
    // at once.
    assert!(is_order_frozen(&FreezeInputs {
        is_window_active: true,
        since_real_pointer_move: Some(Duration::from_secs(30)),
        ..pointing
    }));
    assert!(!is_order_frozen(&FreezeInputs {
        since_real_pointer_move: Some(Duration::from_secs(30)),
        ..pointing
    }));
}

#[test]
fn detail_sidecar_freezes_while_shown_or_pointed_at() {
    let moving = FreezeInputs {
        since_real_pointer_move: recently(),
        ..idle_freeze_inputs()
    };
    // On the sidecar, outside the list.
    assert!(is_order_frozen(&FreezeInputs {
        is_pointer_over_detail_sidecar: true,
        ..moving
    }));
    // On the way from the row to the sidecar, while it's shown.
    assert!(is_order_frozen(&FreezeInputs {
        is_detail_sidecar_shown: true,
        ..moving
    }));
    let on_sidecar = FreezeInputs {
        is_detail_sidecar_shown: true,
        is_pointer_over_detail_sidecar: true,
        ..moving
    };
    // The same limits as the list: resting in the active window keeps it
    // frozen up to the cap, while an inactive window releases it soon.
    assert!(is_order_frozen(&FreezeInputs {
        since_real_pointer_move: Some(POINTER_IDLE_RELEASE),
        ..on_sidecar
    }));
    for inputs in [
        FreezeInputs {
            since_real_pointer_move: Some(POINTER_IDLE_CAP),
            ..on_sidecar
        },
        FreezeInputs {
            is_window_active: false,
            since_real_pointer_move: Some(POINTER_IDLE_RELEASE),
            ..on_sidecar
        },
    ] {
        assert!(!is_order_frozen(&inputs), "{inputs:?}");
    }
}

#[test]
fn keyboard_navigation_freezes_until_another_interaction_or_timeout() {
    let navigating = FreezeInputs {
        since_keyboard_step: Some(Duration::from_secs(3)),
        ..idle_freeze_inputs()
    };
    // Unlike pointing, it doesn't depend on the pointer or the window.
    assert!(is_order_frozen(&navigating));
    assert!(is_order_frozen(&FreezeInputs {
        is_window_active: false,
        ..navigating
    }));
    // A real pointer move after the step ends it.
    assert!(!is_order_frozen(&FreezeInputs {
        has_pointer_moved_since_keyboard_step: true,
        ..navigating
    }));
    // So does the safety timeout.
    assert!(!is_order_frozen(&FreezeInputs {
        since_keyboard_step: Some(KEYBOARD_NAVIGATION_FREEZE_TIMEOUT),
        ..navigating
    }));
    // Other interactions end it by clearing the step.
    assert!(!is_order_frozen(&FreezeInputs {
        since_keyboard_step: None,
        ..navigating
    }));
}

#[test]
fn context_menu_and_pane_drag_freeze() {
    assert!(is_order_frozen(&FreezeInputs {
        is_context_menu_open: true,
        ..idle_freeze_inputs()
    }));
    assert!(is_order_frozen(&FreezeInputs {
        is_dragging_pane: true,
        ..idle_freeze_inputs()
    }));
}

#[test]
fn navigation_target_steps_and_wraps() {
    use StatusRowNavigation::{Last, Next, Number, Previous};
    assert_eq!(navigation_target(Next, Some(0), 3), Some(1));
    assert_eq!(navigation_target(Next, Some(2), 3), Some(0));
    assert_eq!(navigation_target(Previous, Some(1), 3), Some(0));
    assert_eq!(navigation_target(Previous, Some(0), 3), Some(2));
    assert_eq!(navigation_target(Last, Some(0), 3), Some(2));
    assert_eq!(navigation_target(Number(1), Some(2), 3), Some(0));
    assert_eq!(navigation_target(Number(3), None, 3), Some(2));
    // A number past the last row (or zero) lands nowhere.
    assert_eq!(navigation_target(Number(4), Some(0), 3), None);
    assert_eq!(navigation_target(Number(0), Some(0), 3), None);
}

#[test]
fn navigation_target_handles_a_hidden_current_row_and_no_rows() {
    use StatusRowNavigation::{Last, Next, Number, Previous};
    // The focused row isn't shown (e.g. filtered out by the search).
    assert_eq!(navigation_target(Next, None, 3), Some(0));
    assert_eq!(navigation_target(Previous, None, 3), Some(2));
    assert_eq!(navigation_target(Next, Some(7), 3), Some(0));
    assert_eq!(navigation_target(Previous, Some(7), 3), Some(2));
    for navigation in [Next, Previous, Last, Number(1)] {
        assert_eq!(navigation_target(navigation, Some(0), 0), None);
        assert_eq!(navigation_target(navigation, None, 0), None);
    }
}

#[test]
fn group_by_project_navigation_walks_each_group_in_turn() {
    let rows = [
        StatusSortInput {
            project: Some("api"),
            ..row(key(1), AttentionBucket::NeedsYou)
        },
        StatusSortInput {
            project: Some("web"),
            ..row(key(2), AttentionBucket::Unread)
        },
        StatusSortInput {
            project: Some("api"),
            fallback_rank: (2, 0),
            ..row(key(3), AttentionBucket::Idle)
        },
        StatusSortInput {
            project: Some("web"),
            ..row(key(4), AttentionBucket::Running)
        },
        StatusSortInput {
            project: None,
            fallback_rank: (1, 0),
            ..row(key(5), AttentionBucket::Idle)
        },
    ];
    let status_order = status_sorted_indices(&rows);
    assert_eq!(status_order, vec![0, 1, 3, 4, 2]);

    // Navigation walks the rows as displayed: group by group, with groups
    // ordered by their most urgent row, not in plain status order.
    let displayed: Vec<usize> = group_by_project(&rows, &status_order)
        .into_iter()
        .flat_map(|group| group.row_indices)
        .collect();
    assert_eq!(displayed, vec![0, 2, 1, 3, 4]);

    let mut visited = Vec::new();
    let mut current = Some(displayed.len() - 1);
    for _ in 0..displayed.len() {
        current = navigation_target(StatusRowNavigation::Next, current, displayed.len());
        visited.extend(current.and_then(|position| displayed.get(position).copied()));
    }
    assert_eq!(visited, displayed, "every row once, in display order");
}
