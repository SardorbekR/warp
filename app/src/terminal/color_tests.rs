use warpui::color::ColorU;

use super::*;

fn rgb(r: u8, g: u8, b: u8) -> ColorU {
    ColorU::new(r, g, b, 255)
}

#[test]
fn dim_on_black_scales_the_color() {
    assert_eq!(dim(rgb(200, 100, 50), ColorU::black()), rgb(132, 66, 33));
}

#[test]
fn dim_on_white_makes_dark_text_fainter() {
    // Scaling toward black would turn this into rgb(19, 19, 19), darker than the text itself.
    assert_eq!(dim(rgb(30, 30, 30), ColorU::white()), rgb(106, 106, 106));
}

#[test]
fn dim_keeps_the_alpha() {
    assert_eq!(dim(ColorU::new(40, 200, 40, 128), rgb(10, 10, 10)).a, 128);
}

#[test]
fn named_dims_blend_toward_the_terminal_background() {
    let background = rgb(245, 246, 249);
    let foreground = rgb(14, 15, 19);
    let mut colors = Colors::with_foreground_background_color(foreground, background);
    colors.normal.red = rgb(200, 100, 50);
    let list = List::from(&colors);

    assert_eq!(
        list[color_index::DIM_FOREGROUND],
        dim(foreground, background)
    );
    assert_eq!(list[color_index::DIM_FOREGROUND], rgb(92, 93, 97));
    assert_eq!(
        list[color_index::DIM_RED],
        dim(rgb(200, 100, 50), background)
    );
}

#[test]
fn theme_dims_win_over_computed_ones() {
    let dim_foreground = rgb(1, 2, 3);
    let mut colors = Colors::with_foreground_background_color(rgb(14, 15, 19), ColorU::white());
    colors.primary.dim_foreground = Some(dim_foreground);
    let list = List::from(&colors);

    assert_eq!(list[color_index::DIM_FOREGROUND], dim_foreground);
}
