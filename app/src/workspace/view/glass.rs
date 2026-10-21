//! Glass sidebar (fork feature, macOS). With vertical tabs, the window header
//! and the vertical tabs panel are translucent over a blurred desktop, while
//! the tab content and every other panel stay opaque, as one block with a
//! rounded corner next to the tabs panel.
//!
//! The window paints two layers that cover everything (see
//! `Workspace::render`). With glass on, the full-window layer is a
//! translucent tint and the terminal background is painted only behind the
//! opaque block, so that only the header and the tabs panel show the blur.

use warp_core::features::FeatureFlag;
use warp_core::ui::theme::{ColorScheme, WarpTheme};
use warpui::elements::{
    Container, CornerRadius, Element, Fill as ElementFill, Flex, ParentElement, Radius, Shrinkable,
};
use warpui::{AppContext, SingletonEntity, WindowId};

use super::PanelPosition;
use crate::appearance::Appearance;
use crate::window_settings::WindowSettings;
use crate::workspace::header_toolbar_item::HeaderToolbarItemKind;
use crate::workspace::tab_settings::TabSettings;
use crate::workspace::util::get_terminal_background_fill;

/// The window blur while glass is on, unless the user picked a stronger one.
pub(crate) const GLASS_BLUR_RADIUS: u8 = 30;
/// Opacity of the theme tint over the blurred desktop, in percent.
const GLASS_TINT_OPACITY: u8 = 70;
/// Opacity of the light glass tint (see [`is_light_glass`]), in percent.
const LIGHT_GLASS_TINT_OPACITY: u8 = 72;
/// Radius of the opaque block's top corner next to the tabs panel.
const CONTENT_CORNER_RADIUS: f32 = 10.;

/// Whether the glass setting is on. Glass needs the macOS window blur, so it
/// is never on elsewhere.
pub(crate) fn is_glass_enabled(app: &AppContext) -> bool {
    cfg!(target_os = "macos") && *TabSettings::as_ref(app).vertical_tabs_glass
}

/// The blur radius to apply to windows for the user's own `user_blur_radius`:
/// raised to [`GLASS_BLUR_RADIUS`] while glass is on.
pub(crate) fn effective_blur_radius(user_blur_radius: u8, app: &AppContext) -> u8 {
    if is_glass_enabled(app) {
        user_blur_radius.max(GLASS_BLUR_RADIUS)
    } else {
        user_blur_radius
    }
}

/// How a window renders with glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GlassLayout {
    /// The side the tabs panel sits on, as the outermost panel there.
    pub(super) tabs_panel_side: PanelPosition,
}

/// The glass layout for the current settings, or `None` to render the
/// regular (upstream) way. `hide_vertical_tabs` is set for views that never
/// show the vertical tabs panel.
pub(super) fn glass_layout(hide_vertical_tabs: bool, app: &AppContext) -> Option<GlassLayout> {
    let tab_settings = TabSettings::as_ref(app);
    if hide_vertical_tabs
        || !is_glass_enabled(app)
        || !FeatureFlag::VerticalTabs.is_enabled()
        || !*tab_settings.use_vertical_tabs
    {
        return None;
    }
    // A theme's background image already fills the window; keep that look.
    if Appearance::as_ref(app).theme().background_image().is_some() {
        return None;
    }
    let config = &tab_settings.header_toolbar_chip_selection;
    tabs_panel_side(&config.left_items(), &config.right_items())
        .map(|tabs_panel_side| GlassLayout { tabs_panel_side })
}

/// The side of the tabs panel, if it is the outermost panel on that side (the
/// first on the left or the last on the right), so that everything else forms
/// one opaque block next to it.
fn tabs_panel_side(
    left_items: &[HeaderToolbarItemKind],
    right_items: &[HeaderToolbarItemKind],
) -> Option<PanelPosition> {
    if left_items.first() == Some(&HeaderToolbarItemKind::TabsPanel) {
        Some(PanelPosition::Left)
    } else if right_items.last() == Some(&HeaderToolbarItemKind::TabsPanel) {
        Some(PanelPosition::Right)
    } else {
        None
    }
}

/// The fill of the whole window under glass: the theme's surface tint (its
/// own background for light glass), never more opaque than the user's window
/// opacity.
pub(super) fn window_tint(window_id: WindowId, app: &AppContext) -> ElementFill {
    let background_opacity = WindowSettings::as_ref(app)
        .background_opacity
        .effective_opacity(window_id, app);
    let theme = Appearance::as_ref(app).theme();
    let (tint, opacity) = if is_light_glass(theme) {
        (theme.background(), LIGHT_GLASS_TINT_OPACITY)
    } else {
        (theme.surface_2(), GLASS_TINT_OPACITY)
    };
    tint.with_opacity(opacity.min(background_opacity)).into()
}

/// Whether glass on `theme` is light glass: the tint is the theme's own
/// background, and the tabs panel paints no overlay over it, so that the panel
/// looks frosted white on a light theme rather than grey.
pub(super) fn is_light_glass(theme: &WarpTheme) -> bool {
    theme.inferred_color_scheme() == ColorScheme::DarkOnLight
}

/// Whether the vertical tabs panel shows the light glass tint as it is (see
/// [`is_light_glass`]): glass is on for its window and the theme is light.
pub(super) fn tabs_panel_shows_light_glass(app: &AppContext) -> bool {
    glass_layout(false, app).is_some() && is_light_glass(Appearance::as_ref(app).theme())
}

/// Lays out the tabs panel (if open) next to `rest`, which holds the tab
/// content and every other panel. `rest` gets the two background layers the
/// whole window has without glass, so it looks exactly as before.
pub(super) fn render_glass_panels(
    tabs_panel: Option<Box<dyn Element>>,
    rest: Box<dyn Element>,
    layout: GlassLayout,
    window_id: WindowId,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = Appearance::as_ref(app).theme();
    let background_opacity = WindowSettings::as_ref(app)
        .background_opacity
        .effective_opacity(window_id, app);
    // Only round the corner that meets the tabs panel.
    let corner_radius = match (&tabs_panel, layout.tabs_panel_side) {
        (None, _) => CornerRadius::default(),
        (Some(_), PanelPosition::Left) => {
            CornerRadius::with_top_left(Radius::Pixels(CONTENT_CORNER_RADIUS))
        }
        (Some(_), PanelPosition::Right) => {
            CornerRadius::with_top_right(Radius::Pixels(CONTENT_CORNER_RADIUS))
        }
    };
    let opaque_block = Container::new(
        Container::new(rest)
            .with_background(get_terminal_background_fill(window_id, app))
            .with_corner_radius(corner_radius)
            .finish(),
    )
    .with_background(theme.surface_2().with_opacity(background_opacity))
    .with_corner_radius(corner_radius)
    .finish();
    let opaque_block = Shrinkable::new(1.0, opaque_block).finish();

    let mut row = Flex::row();
    match (tabs_panel, layout.tabs_panel_side) {
        (Some(tabs_panel), PanelPosition::Left) => {
            row.add_child(tabs_panel);
            row.add_child(opaque_block);
        }
        (Some(tabs_panel), PanelPosition::Right) => {
            row.add_child(opaque_block);
            row.add_child(tabs_panel);
        }
        (None, _) => row.add_child(opaque_block),
    }
    row.finish()
}

#[cfg(test)]
#[path = "glass_tests.rs"]
mod tests;
