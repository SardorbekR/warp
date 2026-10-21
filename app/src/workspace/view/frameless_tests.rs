use super::*;

/// A window with the tabs panel open on the left of a macOS window.
fn panel_open_on_the_left() -> TopBarInputs {
    TopBarInputs {
        is_setting_on: true,
        has_native_traffic_lights_on_the_left: true,
        is_tabs_panel_open_on_the_left: true,
        shows_decorations_on_hover: false,
        needs_regular_bar: false,
    }
}

#[test]
fn the_top_bar_hides_while_the_tabs_panel_is_open_on_the_left() {
    assert!(hides_top_bar(panel_open_on_the_left()));
}

#[test]
fn the_regular_bar_is_back_whenever_its_buttons_would_be_out_of_reach() {
    // Hidden or moved panel: the traffic lights would cover the tab content
    // or another panel.
    assert!(!hides_top_bar(TopBarInputs {
        is_tabs_panel_open_on_the_left: false,
        ..panel_open_on_the_left()
    }));
    // Traffic lights drawn by Warp itself on the right (Linux, Windows), or a
    // native window frame.
    assert!(!hides_top_bar(TopBarInputs {
        has_native_traffic_lights_on_the_left: false,
        ..panel_open_on_the_left()
    }));
    // The bar only shows on hover, with the traffic lights.
    assert!(!hides_top_bar(TopBarInputs {
        shows_decorations_on_hover: true,
        ..panel_open_on_the_left()
    }));
    // A dragged-out tab, onboarding, or the tab configs chip.
    assert!(!hides_top_bar(TopBarInputs {
        needs_regular_bar: true,
        ..panel_open_on_the_left()
    }));
}

#[test]
fn turning_the_setting_off_brings_the_bar_back() {
    assert!(!hides_top_bar(TopBarInputs {
        is_setting_on: false,
        ..panel_open_on_the_left()
    }));
}
