use pathfinder_color::ColorU;

use super::*;

#[test]
fn dim_true_color_text_blends_toward_the_terminal_background() {
    let colors = color::List::from(&color::Colors::with_foreground_background_color(
        ColorU::black(),
        ColorU::white(),
    ));
    let mut override_colors = color::OverrideList::empty();
    let foreground = Color::Spec(ColorU::new(30, 30, 30, 255));

    assert_eq!(
        compute_fg_rgb(&colors, &override_colors, foreground, Flags::DIM),
        ColorU::new(106, 106, 106, 255)
    );
    assert_eq!(
        compute_fg_rgb(&colors, &override_colors, foreground, Flags::empty()),
        ColorU::new(30, 30, 30, 255)
    );

    // A background set by the running program (OSC 11) is the one dim text blends toward.
    override_colors[color_index::BACKGROUND] = Some(ColorU::black());
    assert_eq!(
        compute_fg_rgb(&colors, &override_colors, foreground, Flags::DIM),
        ColorU::new(19, 19, 19, 255)
    );
}
