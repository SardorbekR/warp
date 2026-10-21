use std::collections::HashSet;
use std::time::Duration;

use pathfinder_color::ColorU;
use warpui::EntityId;

use super::{
    AttentionBucket, DARK_FILL, DEEP_TEXT, FreezeInputs, GroupHeaderText, GroupTransition,
    KEYBOARD_NAVIGATION_FREEZE_TIMEOUT, LIGHT_FILL, OTHER_PROJECT_LABEL, PALE_TEXT,
    POINTER_IDLE_CAP, POINTER_IDLE_RELEASE, PROJECT_COLOR_NAMES, PROJECT_HUES, ProjectGroup,
    StatusRowKey, StatusRowNavigation, StatusSortInput, Tone, activity_age, attention_bucket,
    auto_group_by_project, automatic_project_hue, badge_overhang, bounded_setting,
    compact_age_label, dash_runs, group_by_project, group_header_text, group_transitions,
    is_order_frozen, navigation_target, oklch, picked_project_color, project_color_index,
    project_color_of_hue, project_hue, project_name, project_palette_index, project_pill_style,
    reconcile_frozen_order, resolve_display_order, row_attention_bucket, row_display_status,
    slide_progress, status_sorted_indices,
};
use crate::ai::agent::conversation::ConversationStatus;
use crate::pane_group::{PaneId, TerminalPaneId};
use crate::workspace::project_labels::{
    ProjectLabelTone, ProjectLabelTones, VerticalTabsProjectColors, VerticalTabsProjectLabels,
};

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

/// The WCAG relative luminance of an opaque color (alpha is ignored).
fn relative_luminance(color: ColorU) -> f32 {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.;
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// The WCAG contrast ratio of two opaque colors.
fn contrast_ratio(a: ColorU, b: ColorU) -> f32 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn rgb(r: u8, g: u8, b: u8) -> ColorU {
    ColorU::new(r, g, b, 0xFF)
}

/// No `appearance.vertical_tabs.project_labels` in the settings file.
fn built_in() -> VerticalTabsProjectLabels {
    VerticalTabsProjectLabels::default()
}

#[test]
fn oklch_matches_known_colors() {
    assert_eq!(oklch(1., 0., 0.), ColorU::white());
    assert_eq!(oklch(0., 0., 0.), ColorU::black());
    // The quiet tint's tones, as the agreed mockup draws them.
    assert_eq!(DEEP_TEXT.of(12.), rgb(0x83, 0x3B, 0x46));
    assert_eq!(PALE_TEXT.of(198.), rgb(0x89, 0xDB, 0xDD));
    assert_eq!(DARK_FILL[0].of(12.), rgb(0x53, 0x37, 0x3A));
    assert_eq!(LIGHT_FILL[1].of(256.), rgb(0xDB, 0xE9, 0xFC));
    // Deep teal lies just outside sRGB; its red channel is clipped to zero.
    assert_eq!(DEEP_TEXT.of(198.), rgb(0x00, 0x65, 0x69));
}

#[test]
fn project_label_text_is_readable_on_every_hue() {
    for hue in PROJECT_HUES {
        for dark_theme in [true, false] {
            let style = project_pill_style(hue, dark_theme, &built_in());
            for fill in [style.fill_top, style.fill_bottom] {
                assert_eq!(fill.a, 0xFF, "the label is solid");
                let ratio = contrast_ratio(style.text, fill);
                assert!(
                    ratio >= 4.5,
                    "hue {hue} dark_theme={dark_theme}: {ratio:.2}"
                );
            }
        }
    }
}

#[test]
fn project_tones_are_equally_bright_across_hues() {
    for tone in [
        DEEP_TEXT,
        PALE_TEXT,
        DARK_FILL[0],
        DARK_FILL[1],
        LIGHT_FILL[0],
        LIGHT_FILL[1],
    ] {
        let luminances: Vec<f32> = PROJECT_HUES
            .iter()
            .map(|&hue| relative_luminance(tone.of(hue)))
            .collect();
        let brightest = luminances.iter().copied().fold(f32::MIN, f32::max);
        let dimmest = luminances.iter().copied().fold(f32::MAX, f32::min);
        assert!(brightest / dimmest < 1.25, "{tone:?}: {luminances:?}");
    }
}

#[test]
fn project_label_style_follows_the_theme() {
    let hue = PROJECT_HUES[5];
    // A quiet tint: one flat fill, no glass edge, no shadow; the color is in
    // the text, bright on dark themes and deep on light ones.
    let dark = project_pill_style(hue, true, &built_in());
    assert_eq!(dark.text, PALE_TEXT.of(hue));
    assert_eq!(dark.fill_top, dark.fill_bottom);
    assert_eq!(
        (dark.rim_top.a, dark.rim_bottom.a, dark.shadow.a),
        (0, 0, 0)
    );
    assert!(relative_luminance(dark.text) > relative_luminance(dark.fill_top));

    let light = project_pill_style(hue, false, &built_in());
    assert_eq!(light.text, DEEP_TEXT.of(hue));
    assert_eq!(light.fill_top, light.fill_bottom);
    assert_eq!(
        (light.rim_top.a, light.rim_bottom.a, light.shadow.a),
        (0, 0, 0)
    );
    assert!(relative_luminance(light.text) < relative_luminance(light.fill_top));
}

#[test]
fn built_in_project_labels_keep_their_look() {
    // The quiet tint Sardor picked (v8), value for value, when the settings
    // file overrides nothing.
    let hue = PROJECT_HUES[2];
    assert_eq!(
        project_pill_style(hue, true, &built_in()),
        super::ProjectPillStyle {
            fill_top: oklch(0.37, 0.04, hue),
            fill_bottom: oklch(0.37, 0.04, hue),
            rim_top: ColorU::new(255, 255, 255, 0),
            rim_bottom: ColorU::new(255, 255, 255, 0),
            text: oklch(0.84, 0.08, hue),
            shadow: ColorU::new(0, 0, 0, 0),
            shadow_blur: 1.5,
        }
    );
    let deep = oklch(0.45, 0.1, hue);
    assert_eq!(
        project_pill_style(hue, false, &built_in()),
        super::ProjectPillStyle {
            fill_top: oklch(0.93, 0.03, hue),
            fill_bottom: oklch(0.93, 0.03, hue),
            rim_top: ColorU::new(255, 255, 255, 0),
            rim_bottom: ColorU { a: 0, ..deep },
            text: deep,
            shadow: ColorU::new(31, 35, 48, 0),
            shadow_blur: 1.,
        }
    );
    // A lone row's 16 px badge straddles the row's top edge.
    assert_eq!(badge_overhang(10., 1., 1.), 8.);
    // Sardor's projects keep the slots they had.
    for (project, slot) in [
        ("Shipzen", 0),
        ("Lucid Notes", 7),
        ("Coach-Journal", 3),
        ("warp-custom", 5),
        ("resume", 2),
    ] {
        assert_eq!(
            automatic_project_hue(project, &built_in()),
            PROJECT_HUES[slot],
            "{project}"
        );
    }
}

#[test]
fn settings_file_hues_replace_the_palette() {
    let palette = |hues: Vec<f64>| VerticalTabsProjectLabels {
        hues: Some(hues),
        ..Default::default()
    };
    // Hues wrap into 0 to 360.
    let wrapped = palette(vec![390., -60.]);
    for project in ["Shipzen", "Lucid Notes", "warp", ""] {
        let expected = [30., 300.][project_palette_index(project, 2)];
        assert_eq!(
            automatic_project_hue(project, &wrapped),
            expected,
            "{project}"
        );
    }
    // Non-finite hues are skipped; an empty palette falls back to the built-in one.
    let only_bad = palette(vec![f64::NAN, f64::INFINITY]);
    let empty = palette(Vec::new());
    for project in ["Shipzen", "resume"] {
        let built_in_hue = automatic_project_hue(project, &built_in());
        assert_eq!(
            automatic_project_hue(project, &only_bad),
            built_in_hue,
            "{project}"
        );
        assert_eq!(
            automatic_project_hue(project, &empty),
            built_in_hue,
            "{project}"
        );
    }
    assert_eq!(
        automatic_project_hue("x", &palette(vec![f64::NAN, 42.])),
        42.,
        "the one finite hue is the whole palette"
    );
}

#[test]
fn settings_file_project_hues_win() {
    let mut labels = VerticalTabsProjectLabels {
        hues: Some(vec![10., 20.]),
        ..Default::default()
    };
    labels.project_hues.insert("Shipzen".to_owned(), 725.);
    labels.project_hues.insert("resume".to_owned(), f64::NAN);
    assert_eq!(automatic_project_hue("Shipzen", &labels), 5.);
    // A non-finite pick is ignored, so the project keeps its palette slot.
    assert_eq!(
        automatic_project_hue("resume", &labels),
        [10., 20.][project_palette_index("resume", 2)]
    );
    // Names match exactly.
    assert_eq!(
        automatic_project_hue("shipzen", &labels),
        [10., 20.][project_palette_index("shipzen", 2)]
    );
}

fn colors(entries: &[(&str, &str)]) -> VerticalTabsProjectColors {
    VerticalTabsProjectColors(
        entries
            .iter()
            .map(|(project, color)| ((*project).to_owned(), (*color).to_owned()))
            .collect(),
    )
}

#[test]
fn color_names_match_the_palette() {
    assert_eq!(PROJECT_COLOR_NAMES.len(), PROJECT_HUES.len());
    for (index, name) in PROJECT_COLOR_NAMES.iter().enumerate() {
        assert_eq!(project_color_index(name), Some(index), "{name}");
        assert_eq!(
            project_color_of_hue(PROJECT_HUES[index]),
            Some(index),
            "{name}"
        );
    }
    assert_eq!(project_color_index("rose"), Some(0));
    assert_eq!(project_color_index("lavender"), Some(7));
    // Hand-edited names: case and surrounding spaces don't matter.
    assert_eq!(project_color_index(" Sage "), Some(4));
    assert_eq!(project_color_index("TEAL"), Some(5));
    // Anything else is no color at all.
    for name in ["", "pink", "sage green", "#ff0000", "12"] {
        assert_eq!(project_color_index(name), None, "{name:?}");
    }
    assert_eq!(project_color_of_hue(13.), None);
}

#[test]
fn a_picked_color_wins_and_follows_the_theme() {
    let mut labels = VerticalTabsProjectLabels {
        hues: Some(vec![10., 20.]),
        ..Default::default()
    };
    labels.project_hues.insert("Shipzen".to_owned(), 90.);
    let picked = colors(&[("Shipzen", "sage"), ("resume", "Slate")]);
    // The pick beats the settings file's hues and the palette.
    assert_eq!(project_hue("Shipzen", &labels, &picked), PROJECT_HUES[4]);
    assert_eq!(project_hue("resume", &labels, &picked), PROJECT_HUES[6]);
    assert_eq!(picked_project_color("Shipzen", &picked), Some(4));
    // Projects without a pick keep their automatic hue.
    assert_eq!(
        project_hue("Lucid Notes", &labels, &picked),
        automatic_project_hue("Lucid Notes", &labels)
    );
    assert_eq!(picked_project_color("Lucid Notes", &picked), None);
    // The pick is a name, so the label takes that color's dark or light tones.
    let hue = project_hue("Shipzen", &built_in(), &picked);
    assert_eq!(
        project_pill_style(hue, true, &built_in()).text,
        PALE_TEXT.of(PROJECT_HUES[4])
    );
    assert_eq!(
        project_pill_style(hue, false, &built_in()).text,
        DEEP_TEXT.of(PROJECT_HUES[4])
    );
}

#[test]
fn an_unknown_or_missing_pick_means_automatic() {
    let labels = built_in();
    let none = VerticalTabsProjectColors::default();
    let unknown = colors(&[("Shipzen", "pink"), ("resume", "")]);
    for project in ["Shipzen", "resume", "Lucid Notes"] {
        let automatic = automatic_project_hue(project, &labels);
        assert_eq!(project_hue(project, &labels, &none), automatic, "{project}");
        assert_eq!(
            project_hue(project, &labels, &unknown),
            automatic,
            "{project}"
        );
        assert_eq!(picked_project_color(project, &unknown), None, "{project}");
    }
    // Names match the project exactly.
    let picked = colors(&[("Shipzen", "sage")]);
    assert_eq!(
        project_hue("shipzen", &labels, &picked),
        automatic_project_hue("shipzen", &labels)
    );
}

#[test]
fn picking_and_resetting_a_color_changes_only_that_project() {
    let start = colors(&[("Shipzen", "sage"), ("resume", "teal")]);
    let picked = start.with_color("Lucid Notes", Some("rose"));
    assert_eq!(
        picked,
        colors(&[
            ("Shipzen", "sage"),
            ("resume", "teal"),
            ("Lucid Notes", "rose")
        ])
    );
    let changed = picked.with_color("Shipzen", Some("amber"));
    assert_eq!(changed.color_name("Shipzen"), Some("amber"));
    assert_eq!(changed.color_name("resume"), Some("teal"));
    let reset = changed.with_color("Shipzen", None);
    assert_eq!(
        reset,
        colors(&[("resume", "teal"), ("Lucid Notes", "rose")])
    );
    // Resetting a project without a pick changes nothing.
    assert_eq!(reset.with_color("Shipzen", None), reset);
    // The original is untouched.
    assert_eq!(start, colors(&[("Shipzen", "sage"), ("resume", "teal")]));
}

#[test]
fn settings_file_tones_override_the_built_in_ones_within_bounds() {
    let hue = PROJECT_HUES[6];
    let labels = VerticalTabsProjectLabels {
        dark: ProjectLabelTones {
            // Only the lightness, and too high: clamped to 1, chroma kept.
            text: Some(ProjectLabelTone {
                lightness: Some(2.),
                chroma: None,
            }),
            fill_top: Some(ProjectLabelTone {
                lightness: Some(0.5),
                chroma: Some(1.),
            }),
            fill_bottom: Some(ProjectLabelTone {
                lightness: Some(f64::NAN),
                chroma: Some(-1.),
            }),
            edge_top: Some(0.5),
            edge_bottom: Some(-3.),
            shadow: Some(7.),
            shadow_blur: Some(100.),
        },
        light: ProjectLabelTones {
            text: Some(ProjectLabelTone {
                lightness: None,
                chroma: Some(0.1),
            }),
            edge_bottom: Some(1.),
            shadow_blur: Some(f64::INFINITY),
            ..Default::default()
        },
        ..Default::default()
    };

    let dark = project_pill_style(hue, true, &labels);
    let built_in_dark = project_pill_style(hue, true, &built_in());
    assert_eq!(
        dark.text,
        Tone {
            lightness: 1.,
            chroma: PALE_TEXT.chroma,
        }
        .of(hue)
    );
    assert_eq!(
        dark.fill_top,
        Tone {
            lightness: 0.5,
            chroma: 0.4,
        }
        .of(hue)
    );
    assert_eq!(
        dark.fill_bottom,
        Tone {
            lightness: DARK_FILL[1].lightness,
            chroma: 0.,
        }
        .of(hue)
    );
    assert_eq!(dark.rim_top, ColorU::new(255, 255, 255, 128));
    assert_eq!(dark.rim_bottom, ColorU::new(255, 255, 255, 0));
    assert_eq!(dark.shadow, ColorU::new(0, 0, 0, 255));
    assert_eq!(dark.shadow_blur, 8.);
    // The light theme's look is separate.
    assert_eq!(
        project_pill_style(hue, false, &labels).fill_top,
        project_pill_style(hue, false, &built_in()).fill_top
    );
    assert_ne!(dark, built_in_dark);

    let light = project_pill_style(hue, false, &labels);
    let deep = Tone {
        lightness: DEEP_TEXT.lightness,
        chroma: 0.1,
    }
    .of(hue);
    assert_eq!(light.text, deep);
    assert_eq!(light.rim_bottom, ColorU { a: 255, ..deep });
    assert_eq!(
        light.shadow_blur, 1.,
        "a non-finite blur keeps the built-in one"
    );
}

#[test]
fn bounded_setting_clamps_and_falls_back() {
    assert_eq!(bounded_setting(None, 3., 0., 10.), 3.);
    assert_eq!(bounded_setting(Some(4.5), 3., 0., 10.), 4.5);
    assert_eq!(bounded_setting(Some(-1.), 3., 0., 10.), 0.);
    assert_eq!(bounded_setting(Some(11.), 3., 0., 10.), 10.);
    assert_eq!(bounded_setting(Some(f64::NAN), 3., 0., 10.), 3.);
    assert_eq!(bounded_setting(Some(f64::INFINITY), 3., 0., 10.), 3.);
    assert_eq!(bounded_setting(Some(f64::NEG_INFINITY), 3., 0., 10.), 3.);
}

#[test]
fn dashed_lines_start_and_end_with_a_dash() {
    for length in [8., 9.5, 16., 17., 32., 34., 61.25] {
        let runs = dash_runs(length, 3., 2.);
        assert!(runs.len() >= 2, "{length}: {runs:?}");
        assert_eq!(runs.first(), Some(&(0., 3.)), "{length}");
        let (last_start, last_dash) = *runs.last().expect("dashes");
        assert!(
            (last_start + last_dash - length).abs() < 1e-4,
            "{length}: {runs:?}"
        );
        for pair in runs.windows(2) {
            let gap = pair[1].0 - (pair[0].0 + pair[0].1);
            assert!(gap > 1. && gap < 4., "{length}: gap {gap}");
        }
    }
    // A row's branch: 30 px row and the 2 px gap above it.
    let runs = dash_runs(32., 3., 2.);
    assert_eq!(runs.len(), 7);
    // The stub: two dashes and one gap.
    assert_eq!(dash_runs(8., 3., 2.), vec![(0., 3.), (5., 3.)]);
}

#[test]
fn short_or_broken_dashed_lines_stay_safe() {
    assert_eq!(dash_runs(7.9, 3., 2.), vec![(0., 7.9)]);
    assert_eq!(dash_runs(2., 3., 2.), vec![(0., 2.)]);
    assert!(dash_runs(0., 3., 2.).is_empty());
    assert!(dash_runs(-5., 3., 2.).is_empty());
    assert!(dash_runs(f32::NAN, 3., 2.).is_empty());
    assert!(dash_runs(f32::INFINITY, 3., 2.).is_empty());
    assert_eq!(dash_runs(10., 0., 2.), vec![(0., 10.)]);
    assert_eq!(dash_runs(10., 3., f32::NAN), vec![(0., 10.)]);
}

#[test]
fn badge_overhang_is_half_the_badge_height() {
    assert_eq!(badge_overhang(10., 1., 1.), 8.);
    assert_eq!(badge_overhang(20., 0., 0.), 12.);
    assert_eq!(badge_overhang(10., 3., 2.), 11.);
}

#[test]
fn palette_hues_are_distinct_angles() {
    let hues: HashSet<u64> = PROJECT_HUES.iter().map(|hue| hue.to_bits()).collect();
    assert_eq!(hues.len(), PROJECT_HUES.len());
    assert!(PROJECT_HUES.iter().all(|hue| (0. ..360.).contains(hue)));
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
        let index = project_palette_index(project, PROJECT_HUES.len());
        assert!(index < PROJECT_HUES.len(), "{project}: {index}");
        assert_eq!(index, project_palette_index(project, PROJECT_HUES.len()));
        assert_eq!(
            automatic_project_hue(project, &built_in()),
            PROJECT_HUES[index],
            "{project}"
        );
        for len in 1..=20 {
            assert!(
                project_palette_index(project, len) < len,
                "{project}: {len}"
            );
        }
        assert_eq!(project_palette_index(project, 0), 0, "{project}");
    }
}

#[test]
fn palette_spreads_distinct_projects() {
    let names = [
        "warp", "api", "web", "infra", "docs", "mobile", "cli", "sdk",
    ];
    let distinct: std::collections::HashSet<usize> = names
        .into_iter()
        .map(|name| project_palette_index(name, PROJECT_HUES.len()))
        .collect();
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
