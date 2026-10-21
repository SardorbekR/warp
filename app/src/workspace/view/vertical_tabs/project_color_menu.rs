//! Fork feature: the menu that picks a project's label color, opened by
//! right-clicking a group header's name or a lone row's badge in the vertical
//! tabs panel. A pick is saved by color name
//! (`appearance.vertical_tabs.project_colors`), so the label keeps following
//! dark and light themes.

use std::sync::Arc;

use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use ui_components::tooltip::{Params as TooltipParams, Tooltip as TooltipComponent};
use ui_components::{Component as _, Options as ComponentOptions};
use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::ColorScheme;
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning,
    ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Stack,
};
use warpui::fonts::Properties;
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;

use super::status_order::{
    PROJECT_COLOR_NAMES, PROJECT_HUES, automatic_project_hue, picked_project_color,
    project_color_of_hue, project_pill_style,
};
use crate::appearance::Appearance;
use crate::menu::{MenuAction, MenuItem, MenuItemFields};
use crate::ui_components::icons::Icon;
use crate::workspace::action::WorkspaceAction;
use crate::workspace::project_labels::{VerticalTabsProjectColors, VerticalTabsProjectLabels};

/// Wide enough for the eight colors in one row.
pub(in crate::workspace) const PROJECT_COLOR_MENU_WIDTH: f32 = 232.;
const DOT_SIZE: f32 = 14.;
/// The ring around the color in use, and the space between it and the dot.
const RING_WIDTH: f32 = 1.5;
const RING_GAP: f32 = 2.;
/// How strong the ring is: the theme's text color at this opacity.
const RING_OPACITY: u8 = 85;

/// Which of the eight colors a project's label uses, as slots in
/// [`PROJECT_HUES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectColorChoice {
    /// The color the label shows now. `None` when its automatic hue comes
    /// from the settings file and isn't one of the eight.
    pub(super) current: Option<usize>,
    /// Whether the color was picked from the menu, rather than automatic.
    pub(super) picked: bool,
    /// The color "Automatic" gives the project, if it's one of the eight.
    pub(super) automatic: Option<usize>,
}

pub(super) fn project_color_choice(
    project: &str,
    labels: &VerticalTabsProjectLabels,
    colors: &VerticalTabsProjectColors,
) -> ProjectColorChoice {
    let automatic = project_color_of_hue(automatic_project_hue(project, labels));
    match picked_project_color(project, colors) {
        Some(index) => ProjectColorChoice {
            current: Some(index),
            picked: true,
            automatic,
        },
        None => ProjectColorChoice {
            current: automatic,
            picked: false,
            automatic,
        },
    }
}

/// A color's name as the menu shows it: "Rose", "Lavender".
pub(super) fn color_display_name(index: usize) -> Option<String> {
    let name = PROJECT_COLOR_NAMES.get(index)?;
    let mut chars = name.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// "Shipzen color", a row of the eight colors (the one in use ringed, each
/// named on hover), and "Automatic", checked when nothing is picked and naming
/// the color it gives.
pub(in crate::workspace) fn project_color_menu_items(
    project: &str,
    labels: &VerticalTabsProjectLabels,
    colors: &VerticalTabsProjectColors,
    appearance: &Appearance,
) -> Vec<MenuItem<WorkspaceAction>> {
    let theme = appearance.theme();
    let choice = project_color_choice(project, labels, colors);

    let header = MenuItem::Header {
        fields: MenuItemFields::new(format!("{project} color"))
            .with_font_size_override((appearance.ui_font_size() - 1.).max(9.))
            .with_override_text_color(theme.sub_text_color(theme.surface_2()).into_solid())
            .with_clip_config(ClipConfig::ellipsis())
            .with_no_interaction_on_hover(),
        clickable: false,
        right_side_fields: None,
    };

    let mouse_states: Vec<MouseStateHandle> = PROJECT_HUES
        .iter()
        .map(|_| MouseStateHandle::default())
        .collect();
    let dots_project = project.to_owned();
    let dots_labels = labels.clone();
    let dots = MenuItem::Item(
        MenuItemFields::new_with_custom_label(
            Arc::new(move |_is_selected, _is_hovered, appearance, _app| {
                let theme = appearance.theme();
                let dark_theme = theme.inferred_color_scheme() == ColorScheme::LightOnDark;
                let ring = coloru_with_opacity(theme.foreground().into_solid(), RING_OPACITY);
                let mut row = Flex::row()
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_main_axis_size(MainAxisSize::Max);
                for (index, (hue, mouse_state)) in
                    PROJECT_HUES.iter().zip(mouse_states.iter()).enumerate()
                {
                    let (Some(&name), Some(display_name)) =
                        (PROJECT_COLOR_NAMES.get(index), color_display_name(index))
                    else {
                        continue;
                    };
                    // The dot is the label's text color: what the name turns into.
                    let color = project_pill_style(*hue, dark_theme, &dots_labels).text;
                    let project = dots_project.clone();
                    let dot = render_color_choice(
                        mouse_state.clone(),
                        color,
                        choice.current == Some(index),
                        ring,
                        display_name,
                        appearance,
                    )
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(WorkspaceAction::SetProjectColor {
                            project: project.clone(),
                            color: Some(name.to_owned()),
                        });
                        ctx.dispatch_typed_action(MenuAction::Close(true));
                    });
                    row.add_child(dot.finish());
                }
                row.finish()
            }),
            None,
        )
        .no_highlight_on_hover()
        .with_no_interaction_on_hover(),
    );

    let mut automatic =
        MenuItemFields::new("Automatic").with_on_select_action(WorkspaceAction::SetProjectColor {
            project: project.to_owned(),
            color: None,
        });
    automatic = if choice.picked {
        automatic.with_indent()
    } else {
        automatic.with_icon(Icon::Check)
    };
    if let Some(name) = choice.automatic.and_then(color_display_name) {
        automatic = automatic.with_right_side_label(name, Properties::default());
    }

    vec![header, dots, MenuItem::Separator, automatic.into_item()]
}

/// One color in the menu: a dot, ringed when `is_current`, named in a tooltip
/// on hover. Returns the `Hoverable` so the caller can add the click.
fn render_color_choice(
    mouse_state: MouseStateHandle,
    color: ColorU,
    is_current: bool,
    ring: ColorU,
    name: String,
    appearance: &Appearance,
) -> Hoverable {
    let ring = if is_current {
        ring
    } else {
        ColorU::transparent_black()
    };
    Hoverable::new(mouse_state, move |state| {
        let dot = Container::new(
            ConstrainedBox::new(Icon::Ellipse.to_warpui_icon(color.into()).finish())
                .with_width(DOT_SIZE)
                .with_height(DOT_SIZE)
                .finish(),
        )
        .with_uniform_padding(RING_GAP)
        .with_border(Border::all(RING_WIDTH).with_border_color(ring))
        .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
        .finish();
        if !state.is_hovered() {
            return dot;
        }
        let tooltip = TooltipComponent.render(
            appearance,
            TooltipParams {
                label: name.clone().into(),
                options: ComponentOptions::default(appearance),
            },
        );
        let mut stack = Stack::new().with_child(dot);
        stack.add_positioned_child(
            tooltip,
            OffsetPositioning::offset_from_parent(
                vec2f(0., -4.),
                ParentOffsetBounds::Unbounded,
                ParentAnchor::TopMiddle,
                ChildAnchor::BottomMiddle,
            ),
        );
        stack.finish()
    })
    .with_cursor(Cursor::PointingHand)
}

#[cfg(test)]
#[path = "project_color_menu_tests.rs"]
mod tests;
