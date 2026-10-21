use super::super::status_order::project_palette_index;
use super::*;

fn colors(entries: &[(&str, &str)]) -> VerticalTabsProjectColors {
    VerticalTabsProjectColors(
        entries
            .iter()
            .map(|(project, color)| ((*project).to_owned(), (*color).to_owned()))
            .collect(),
    )
}

#[test]
fn colors_are_named_for_the_menu() {
    let names: Vec<String> = (0..PROJECT_HUES.len())
        .filter_map(color_display_name)
        .collect();
    assert_eq!(
        names,
        [
            "Rose", "Clay", "Amber", "Moss", "Sage", "Teal", "Slate", "Lavender"
        ]
    );
    assert_eq!(color_display_name(PROJECT_HUES.len()), None);
}

#[test]
fn an_automatic_color_is_marked_and_named() {
    let labels = VerticalTabsProjectLabels::default();
    let slot = project_palette_index("Shipzen", PROJECT_HUES.len());
    assert_eq!(
        project_color_choice("Shipzen", &labels, &VerticalTabsProjectColors::default()),
        ProjectColorChoice {
            current: Some(slot),
            picked: false,
            automatic: Some(slot),
        }
    );
    // A name the menu doesn't know counts as no pick.
    assert_eq!(
        project_color_choice("Shipzen", &labels, &colors(&[("Shipzen", "pink")])),
        ProjectColorChoice {
            current: Some(slot),
            picked: false,
            automatic: Some(slot),
        }
    );
}

#[test]
fn a_picked_color_is_marked_and_automatic_still_named() {
    let labels = VerticalTabsProjectLabels::default();
    let slot = project_palette_index("Shipzen", PROJECT_HUES.len());
    assert_eq!(
        project_color_choice("Shipzen", &labels, &colors(&[("Shipzen", "sage")])),
        ProjectColorChoice {
            current: Some(4),
            picked: true,
            automatic: Some(slot),
        }
    );
}

#[test]
fn a_custom_automatic_hue_marks_nothing() {
    // The settings file's own hue isn't one of the eight colors.
    let mut labels = VerticalTabsProjectLabels::default();
    labels.project_hues.insert("Shipzen".to_owned(), 90.);
    assert_eq!(
        project_color_choice("Shipzen", &labels, &VerticalTabsProjectColors::default()),
        ProjectColorChoice {
            current: None,
            picked: false,
            automatic: None,
        }
    );
    assert_eq!(
        project_color_choice("Shipzen", &labels, &colors(&[("Shipzen", "teal")])),
        ProjectColorChoice {
            current: Some(5),
            picked: true,
            automatic: None,
        }
    );
}

#[test]
fn the_menu_has_a_title_the_colors_and_automatic() {
    let appearance = Appearance::mock();
    let labels = VerticalTabsProjectLabels::default();
    let automatic_name = color_display_name(project_palette_index("Shipzen", PROJECT_HUES.len()))
        .expect("a palette slot");

    for (picks, checked) in [
        (VerticalTabsProjectColors::default(), true),
        (colors(&[("Shipzen", "sage")]), false),
    ] {
        let items = project_color_menu_items("Shipzen", &labels, &picks, &appearance);
        assert_eq!(items.len(), 4);
        match &items[0] {
            MenuItem::Header {
                fields, clickable, ..
            } => {
                assert_eq!(fields.label(), "Shipzen color");
                assert!(!clickable);
            }
            _ => panic!("the menu opens with its title"),
        }
        match &items[1] {
            MenuItem::Item(fields) => assert!(fields.on_select_action().is_none()),
            _ => panic!("the colors come second"),
        }
        assert!(matches!(items[2], MenuItem::Separator));
        match &items[3] {
            MenuItem::Item(fields) => {
                assert_eq!(fields.label(), "Automatic");
                assert_eq!(fields.icon().is_some(), checked);
                match fields.on_select_action() {
                    Some(WorkspaceAction::SetProjectColor { project, color }) => {
                        assert_eq!(project, "Shipzen");
                        assert_eq!(color, &None);
                    }
                    other => panic!("Automatic resets the color: {other:?}"),
                }
            }
            _ => panic!("Automatic comes last"),
        }
    }
    assert!(!automatic_name.is_empty());
}
