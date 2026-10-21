//! No top bar (fork feature, macOS). While the vertical tabs panel is open as
//! the first panel on the left, the window has no bar across its top: the
//! panel's top row holds the traffic lights and the bar's buttons, and the tab
//! content gets a slim header with the session's name, its folder and the code
//! review button.
//!
//! Both rows sit in the native titlebar band, which keeps the bar's height
//! (see `Workspace::update_titlebar_height`), so their empty space still drags
//! the window and a double-click on it still zooms. Whenever the panel is
//! hidden or elsewhere, the regular bar is back, so its buttons are always
//! reachable and the traffic lights never cover another panel. In full screen
//! the rows stay (there is no bar to hide on its own), without the traffic
//! lights.

use warp_core::context_flag::ContextFlag;
use warp_core::features::FeatureFlag;
use warpui::elements::{
    Align, Border, Clipped, ConstrainedBox, Container, CrossAxisAlignment, DispatchEventResult,
    Element, EventHandler, Expanded, Flex, MainAxisSize, ParentElement, SavePosition, Shrinkable,
    Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, SingletonEntity};

use super::{
    AI_ASSISTANT_BUTTON_ID, TAB_BAR_BORDER_HEIGHT, TAB_BAR_HEIGHT, TAB_BAR_ICON_PADDING,
    TAB_BAR_PADDING_LEFT, TAB_BAR_PADDING_RIGHT, TAB_BAR_POSITION_ID,
    TOGGLE_COMMAND_PALETTE_KEYBINDING_NAME, Workspace, vertical_tabs,
};
use crate::appearance::Appearance;
use crate::network::NetworkStatus;
use crate::palette::PaletteMode;
use crate::server::telemetry::PaletteSource;
use crate::ui_components::icons;
use crate::util::traffic_lights::{TrafficLightSide, traffic_light_data};
use crate::workspace::action::WorkspaceAction;
use crate::workspace::header_toolbar_item::HeaderToolbarItemKind;
use crate::workspace::tab_settings::{TabSettings, WorkspaceDecorationVisibility};

const SESSION_HEADER_TITLE_FONT_SIZE: f32 = 13.;
const SESSION_HEADER_FOLDER_FONT_SIZE: f32 = 12.;
/// Inset of the session header's title from the tab content's left edge.
const SESSION_HEADER_PADDING_LEFT: f32 = 16.;
const SESSION_HEADER_SPACING: f32 = 10.;

/// What decides whether a window has no top bar.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TopBarInputs {
    /// `appearance.vertical_tabs.hide_top_bar`.
    pub(super) is_setting_on: bool,
    /// The traffic lights are the native ones in the window's top-left corner
    /// (macOS without the native window frame).
    pub(super) has_native_traffic_lights_on_the_left: bool,
    /// The vertical tabs panel is open as the first panel on the left.
    pub(super) is_tabs_panel_open_on_the_left: bool,
    /// The bar only shows on hover, which also hides the traffic lights.
    pub(super) shows_decorations_on_hover: bool,
    /// Something needs the regular bar: a tab dragged out into its own
    /// window, or onboarding and the tab configs chip, which point at it.
    pub(super) needs_regular_bar: bool,
}

/// Whether a window has no top bar; see the module docs.
pub(super) fn hides_top_bar(inputs: TopBarInputs) -> bool {
    inputs.is_setting_on
        && inputs.has_native_traffic_lights_on_the_left
        && inputs.is_tabs_panel_open_on_the_left
        && !inputs.shows_decorations_on_hover
        && !inputs.needs_regular_bar
}

impl Workspace {
    /// Whether this window has no top bar; see the module docs.
    pub(super) fn hides_top_bar(&self, app: &AppContext) -> bool {
        let tab_settings = TabSettings::as_ref(app);
        let config = &tab_settings.header_toolbar_chip_selection;
        hides_top_bar(TopBarInputs {
            is_setting_on: *tab_settings.vertical_tabs_hide_top_bar,
            has_native_traffic_lights_on_the_left: cfg!(target_os = "macos")
                && traffic_light_data(app, self.window_id)
                    .is_some_and(|data| data.side == TrafficLightSide::Left),
            is_tabs_panel_open_on_the_left: self.vertical_tabs_panel_open
                && HeaderToolbarItemKind::TabsPanel.is_supported(app)
                && config.left_items().first() == Some(&HeaderToolbarItemKind::TabsPanel),
            shows_decorations_on_hover: tab_settings.workspace_decoration_visibility
                == WorkspaceDecorationVisibility::OnHover,
            needs_regular_bar: self.is_tab_drag_preview
                || self.hoa_onboarding_flow.is_some()
                || self.should_show_session_config_tab_config_chip(),
        })
    }

    /// The vertical tabs panel's top row without the top bar: room for the
    /// traffic lights, the bar's buttons except code review (which the session
    /// header shows), search and settings, with `new_tab_button` at the end.
    /// Its empty space is left to the window, to drag it or zoom it.
    pub(super) fn render_tabs_panel_top_row(
        &self,
        new_tab_button: Box<dyn Element>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let config = TabSettings::as_ref(app)
            .header_toolbar_chip_selection
            .clone();

        let mut buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for item in config.left_items().into_iter().chain(config.right_items()) {
            if item == HeaderToolbarItemKind::CodeReview {
                continue;
            }
            if let Some(button) = self.render_header_toolbar_button(&item, appearance, app) {
                buttons.add_child(button);
            }
        }
        buttons.add_child(
            Container::new(self.render_top_bar_search_button(appearance))
                .with_margin_left(TAB_BAR_ICON_PADDING)
                .finish(),
        );
        if FeatureFlag::AvatarInTabBar.is_enabled() {
            buttons.add_child(
                Container::new(self.render_avatar_button(appearance, app))
                    .with_margin_left(TAB_BAR_PADDING_LEFT)
                    .finish(),
            );
        } else {
            if !self.current_workspace_state.is_resource_center_open
                && ContextFlag::WarpEssentials.is_enabled()
            {
                buttons.add_child(
                    Container::new(self.render_resource_center_button(appearance, app))
                        .with_margin_left(TAB_BAR_PADDING_LEFT)
                        .finish(),
                );
            }
            buttons.add_child(
                Container::new(self.render_settings_button(appearance))
                    .with_margin_left(TAB_BAR_PADDING_LEFT)
                    .finish(),
            );
        }

        // On a narrow panel the buttons are cut off rather than the new tab
        // button.
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Expanded::new(
                    1.,
                    Clipped::new(Align::new(buttons.finish()).left().finish()).finish(),
                )
                .finish(),
            )
            .with_child(new_tab_button)
            .finish();
        self.with_top_bar_behavior(
            ConstrainedBox::new(
                Container::new(row)
                    // The bar's padding, so the buttons keep their place.
                    .with_padding_left(self.compute_tab_bar_left_padding(app))
                    .with_padding_right(TAB_BAR_PADDING_RIGHT)
                    .finish(),
            )
            .with_height(TAB_BAR_HEIGHT)
            .finish(),
        )
    }

    /// Puts the session header above `content`, the active tab's content.
    pub(super) fn with_session_header(
        &self,
        content: Box<dyn Element>,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_session_header(appearance, app))
            .with_child(Shrinkable::new(1., content).finish())
            .finish()
    }

    /// The session's title and folder, then the bar's controls that belong
    /// to the tab content: the update and offline indicators, code review and
    /// the team switcher.
    fn render_session_header(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let theme = appearance.theme();
        let pane_group = self.active_tab_pane_group().as_ref(app);

        let mut text_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(SESSION_HEADER_SPACING);
        if let Some(vertical_tabs::SessionHeaderText {
            title,
            is_monospace,
            folder,
        }) = vertical_tabs::session_header_text(pane_group, app)
        {
            let title_font = if is_monospace {
                appearance.monospace_font_family()
            } else {
                appearance.ui_font_family()
            };
            text_row.add_child(
                Shrinkable::new(
                    2.,
                    Text::new_inline(title, title_font, SESSION_HEADER_TITLE_FONT_SIZE)
                        .with_clip(ClipConfig::end())
                        .with_color(theme.main_text_color(theme.background()).into_solid())
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .finish(),
                )
                .finish(),
            );
            if let Some(folder) = folder {
                // A long path keeps its end, the folder's own name.
                text_row.add_child(
                    Shrinkable::new(
                        1.,
                        Text::new_inline(
                            folder,
                            appearance.ui_font_family(),
                            SESSION_HEADER_FOLDER_FONT_SIZE,
                        )
                        .with_clip(ClipConfig::start())
                        .with_color(theme.sub_text_color(theme.background()).into_solid())
                        .finish(),
                    )
                    .finish(),
                );
            }
        }

        let mut controls = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        if let Some(update_pill) = self.render_tab_overflow_menu(app, appearance) {
            controls.add_child(
                Container::new(update_pill)
                    .with_margin_left(TAB_BAR_PADDING_LEFT)
                    .finish(),
            );
        }
        let is_online = NetworkStatus::as_ref(app).is_online();
        if !is_online {
            controls.add_child(
                Container::new(self.render_offline_button(appearance))
                    .with_margin_right(4.)
                    .finish(),
            );
        }
        let config = TabSettings::as_ref(app)
            .header_toolbar_chip_selection
            .clone();
        if config.contains_item(&HeaderToolbarItemKind::CodeReview)
            && let Some(button) = self.render_header_toolbar_button(
                &HeaderToolbarItemKind::CodeReview,
                appearance,
                app,
            )
        {
            controls.add_child(button);
        }
        let is_web_anonymous_user = self
            .auth_state
            .is_user_web_anonymous_user()
            .unwrap_or_default();
        if is_online
            && !FeatureFlag::AgentMode.is_enabled()
            && !is_web_anonymous_user
            && !self.current_workspace_state.is_ai_assistant_panel_open
        {
            controls.add_child(
                Container::new(
                    SavePosition::new(
                        self.render_legacy_warp_ai_entrypoint_button(appearance),
                        AI_ASSISTANT_BUTTON_ID,
                    )
                    .finish(),
                )
                .with_margin_left(TAB_BAR_PADDING_LEFT)
                .finish(),
            );
        }
        if let Some(pill) = self.render_team_switcher_pill(appearance, app) {
            controls.add_child(pill);
        }

        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(SESSION_HEADER_SPACING)
            .with_child(Expanded::new(1., text_row.finish()).finish())
            .with_child(controls.finish())
            .finish();
        let header = Container::new(
            ConstrainedBox::new(
                Container::new(row)
                    .with_padding_left(SESSION_HEADER_PADDING_LEFT)
                    .with_padding_right(TAB_BAR_PADDING_RIGHT)
                    .finish(),
            )
            .with_height(TAB_BAR_HEIGHT)
            .finish(),
        )
        .with_border(Border::bottom(TAB_BAR_BORDER_HEIGHT).with_border_fill(theme.outline()))
        .finish();
        // Toasts that hang off the bar hang off the header instead.
        SavePosition::new(self.with_top_bar_behavior(header), TAB_BAR_POSITION_ID).finish()
    }

    /// The bar's search box as an icon: it opens the command palette.
    fn render_top_bar_search_button(&self, appearance: &Appearance) -> Box<dyn Element> {
        Align::new(
            self.render_tab_bar_icon_button(
                appearance,
                icons::Icon::Search,
                &self.mouse_states.title_bar_search_bar,
                WorkspaceAction::OpenPalette {
                    mode: PaletteMode::Command,
                    source: PaletteSource::TitleBarSearchBar,
                    query: None,
                },
                "Search sessions, agents, files".to_string(),
                self.cached_keybindings
                    .get(TOGGLE_COMMAND_PALETTE_KEYBINDING_NAME)
                    .cloned()
                    .flatten(),
                false,
                false,
            )
            .finish(),
        )
        .finish()
    }

    /// Like the bar, a right-click opens the menu that picks its buttons.
    /// Left clicks on empty space aren't handled, so they reach the window.
    fn with_top_bar_behavior(&self, row: Box<dyn Element>) -> Box<dyn Element> {
        EventHandler::new(row)
            .on_right_mouse_down(|ctx, _, position, _| {
                ctx.dispatch_typed_action(WorkspaceAction::ShowHeaderToolbarContextMenu {
                    position,
                });
                DispatchEventResult::StopPropagation
            })
            .finish()
    }
}

#[cfg(test)]
#[path = "frameless_tests.rs"]
mod tests;
