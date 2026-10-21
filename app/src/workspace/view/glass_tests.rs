use super::*;

#[test]
fn tabs_panel_side_is_found_only_when_outermost() {
    use HeaderToolbarItemKind::{CodeReview, TabsPanel, ToolsPanel};

    assert_eq!(
        tabs_panel_side(&[TabsPanel, ToolsPanel], &[CodeReview]),
        Some(PanelPosition::Left)
    );
    assert_eq!(
        tabs_panel_side(&[ToolsPanel], &[CodeReview, TabsPanel]),
        Some(PanelPosition::Right)
    );
    // Another panel sits between the tabs panel and the window edge.
    assert_eq!(tabs_panel_side(&[ToolsPanel, TabsPanel], &[]), None);
    assert_eq!(tabs_panel_side(&[], &[TabsPanel, CodeReview]), None);
    // No tabs panel at all.
    assert_eq!(tabs_panel_side(&[ToolsPanel], &[CodeReview]), None);
}

#[test]
fn default_toolbar_puts_the_tabs_panel_outermost_on_the_left() {
    assert_eq!(
        tabs_panel_side(
            &HeaderToolbarItemKind::default_left(),
            &HeaderToolbarItemKind::default_right()
        ),
        Some(PanelPosition::Left)
    );
}

#[test]
fn only_light_themes_get_light_glass() {
    use crate::themes::default_themes::{dark_theme, light_theme};

    assert!(is_light_glass(&light_theme()));
    assert!(!is_light_glass(&dark_theme()));
}
