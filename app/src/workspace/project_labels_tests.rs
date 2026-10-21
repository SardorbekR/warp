use std::collections::HashMap;

use settings::Setting;
use warpui_extras::user_preferences::toml_backed::TomlBackedUserPreferences;

use super::*;

/// Reads the setting from a settings file with `contents`, the way the app
/// does on start and on every change to the file.
fn read_from_settings_file(contents: &str) -> Option<VerticalTabsProjectLabels> {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, contents).expect("write settings file");
    let (preferences, error) = TomlBackedUserPreferences::new(path);
    assert!(error.is_none(), "{error:?}");
    VerticalTabsProjectLabels::read_from_preferences(&preferences)
}

fn tone(lightness: f64, chroma: f64) -> Option<ProjectLabelTone> {
    Some(ProjectLabelTone {
        lightness: Some(lightness),
        chroma: Some(chroma),
    })
}

#[test]
fn project_labels_use_the_vertical_tabs_path() {
    assert_eq!(
        VerticalTabsProjectLabels::toml_path(),
        Some("appearance.vertical_tabs.project_labels")
    );
    assert_eq!(
        VerticalTabsProjectLabels::hierarchy(),
        Some("appearance.vertical_tabs")
    );
    assert_eq!(VerticalTabsProjectLabels::toml_key(), "project_labels");
}

#[test]
fn project_labels_default_to_no_overrides() {
    let labels = VerticalTabsProjectLabels::default();
    assert_eq!(labels.hues, None);
    assert!(labels.project_hues.is_empty());
    assert_eq!(labels.badge, ProjectLabelShape::default());
    assert_eq!(labels.header, ProjectLabelShape::default());
    assert_eq!(labels.edge_width, None);
    assert_eq!(labels.dark, ProjectLabelTones::default());
    assert_eq!(labels.light, ProjectLabelTones::default());
}

#[test]
fn project_labels_are_absent_without_a_table() {
    assert_eq!(
        read_from_settings_file("[appearance.vertical_tabs]\nenabled = true\n"),
        None
    );
}

#[test]
fn every_project_label_value_reads_from_the_settings_file() {
    let labels = read_from_settings_file(
        r#"
[appearance.vertical_tabs.project_labels]
hues = [10, 50.5, 300]
project_hues = { "Lucid Notes" = 200, Shipzen = 30 }
badge = { font_size = 11, padding_x = 7, padding_y = 2, corner_radius = 6 }
header = { font_size = 12.5, padding_x = 8, padding_y = 3, corner_radius = 9 }
edge_width = 1.5

[appearance.vertical_tabs.project_labels.dark]
fill_top = { lightness = 0.45, chroma = 0.06 }
fill_bottom = { lightness = 0.40, chroma = 0.05 }
text = { lightness = 0.9, chroma = 0.04 }
edge_top = 0.4
edge_bottom = 0.1
shadow = 0.3
shadow_blur = 2

[appearance.vertical_tabs.project_labels.light]
text = { lightness = 0.4, chroma = 0.09 }
shadow = 0
"#,
    )
    .expect("valid project labels");

    assert_eq!(labels.hues, Some(vec![10., 50.5, 300.]));
    assert_eq!(
        labels.project_hues,
        HashMap::from([
            ("Lucid Notes".to_owned(), 200.),
            ("Shipzen".to_owned(), 30.)
        ])
    );
    assert_eq!(
        labels.badge,
        ProjectLabelShape {
            font_size: Some(11.),
            padding_x: Some(7.),
            padding_y: Some(2.),
            corner_radius: Some(6.),
        }
    );
    assert_eq!(
        labels.header,
        ProjectLabelShape {
            font_size: Some(12.5),
            padding_x: Some(8.),
            padding_y: Some(3.),
            corner_radius: Some(9.),
        }
    );
    assert_eq!(labels.edge_width, Some(1.5));
    assert_eq!(
        labels.dark,
        ProjectLabelTones {
            fill_top: tone(0.45, 0.06),
            fill_bottom: tone(0.40, 0.05),
            text: tone(0.9, 0.04),
            edge_top: Some(0.4),
            edge_bottom: Some(0.1),
            shadow: Some(0.3),
            shadow_blur: Some(2.),
        }
    );
    assert_eq!(
        labels.light,
        ProjectLabelTones {
            text: tone(0.4, 0.09),
            shadow: Some(0.),
            ..Default::default()
        }
    );
}

#[test]
fn left_out_project_label_values_stay_unset() {
    let labels = read_from_settings_file(
        "[appearance.vertical_tabs.project_labels.dark]\ntext = { lightness = 0.92 }\n",
    )
    .expect("valid project labels");

    assert_eq!(
        labels,
        VerticalTabsProjectLabels {
            dark: ProjectLabelTones {
                text: Some(ProjectLabelTone {
                    lightness: Some(0.92),
                    chroma: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    );
}

#[test]
fn a_mistyped_project_label_value_rejects_the_table() {
    // The app then keeps the built-in look and reports the setting as invalid.
    assert_eq!(
        read_from_settings_file(
            "[appearance.vertical_tabs.project_labels]\nbadge = { font_size = \"big\" }\n"
        ),
        None
    );
    assert_eq!(
        read_from_settings_file("[appearance.vertical_tabs.project_labels]\nhues = 12\n"),
        None
    );
}

#[test]
fn non_finite_project_label_values_never_get_through() {
    // The file's `nan` and `inf` reach the setting as JSON nulls: an optional
    // value reads as unset, and a required one rejects the table.
    let labels = read_from_settings_file(
        "[appearance.vertical_tabs.project_labels]\nedge_width = nan\nbadge = { font_size = inf }\n",
    )
    .expect("valid project labels");
    assert_eq!(labels.edge_width, None);
    assert_eq!(labels.badge.font_size, None);

    assert_eq!(
        read_from_settings_file("[appearance.vertical_tabs.project_labels]\nhues = [10, nan]\n"),
        None
    );
    assert_eq!(
        read_from_settings_file(
            "[appearance.vertical_tabs.project_labels]\nproject_hues = { Shipzen = -inf }\n"
        ),
        None
    );
}
