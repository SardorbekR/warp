//! Fork feature: the look of the project labels in the vertical tabs panel (a
//! lone row's badge and a group header's name), read from the settings file
//! and applied live. Every value is optional; anything left out keeps the
//! built-in look, and out-of-range values are clamped where they are drawn.

use std::collections::HashMap;

use settings::{SupportedPlatforms, SyncToCloud};

use super::tab_settings::{TabSettings, TabSettingsChangedEvent};

#[derive(
    Default,
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(default)]
#[schemars(
    description = "How the project labels in the vertical tabs panel look. Leave a value out to keep the built-in look."
)]
pub struct VerticalTabsProjectLabels {
    /// The palette, as hue angles from 0 to 360. A project's name picks a
    /// slot, so changing the number of hues can move projects to other slots.
    pub hues: Option<Vec<f64>>,
    /// Hue angles for specific projects, by project name.
    pub project_hues: HashMap<String, f64>,
    /// A lone row's badge.
    pub badge: ProjectLabelShape,
    /// A group header's project name.
    pub header: ProjectLabelShape,
    /// Width of the glass edge, in pixels.
    pub edge_width: Option<f64>,
    /// Colors on dark themes.
    pub dark: ProjectLabelTones,
    /// Colors on light themes.
    pub light: ProjectLabelTones,
}

#[derive(
    Default,
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "The size of a project label, in pixels.")]
pub struct ProjectLabelShape {
    pub font_size: Option<f64>,
    /// Space left and right of the name.
    pub padding_x: Option<f64>,
    /// Space above and below the name.
    pub padding_y: Option<f64>,
    pub corner_radius: Option<f64>,
}

#[derive(
    Default,
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "The colors of the project labels on one kind of theme.")]
pub struct ProjectLabelTones {
    /// The label's color at its top edge.
    pub fill_top: Option<ProjectLabelTone>,
    /// The label's color at its bottom edge.
    pub fill_bottom: Option<ProjectLabelTone>,
    pub text: Option<ProjectLabelTone>,
    /// How strong the glass edge is at the top, from 0 to 1.
    pub edge_top: Option<f64>,
    /// How strong the glass edge is at the bottom, from 0 to 1.
    pub edge_bottom: Option<f64>,
    /// How dark the shadow under the label is, from 0 to 1.
    pub shadow: Option<f64>,
    /// How soft the shadow is, in pixels.
    pub shadow_blur: Option<f64>,
}

#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "A shade of the project's hue, in OKLCH terms.")]
pub struct ProjectLabelTone {
    /// From 0 (black) to 1 (white).
    pub lightness: Option<f64>,
    /// How colorful, from 0 (grey) to about 0.37.
    pub chroma: Option<f64>,
}

settings::macros::implement_setting_for_enum!(
    VerticalTabsProjectLabels,
    TabSettings,
    SupportedPlatforms::ALL,
    // Fork-only setting: never synced to the user's Warp account.
    SyncToCloud::Never,
    surface: settings::SettingSurfaces::GUI,
    private: false,
    toml_path: "appearance.vertical_tabs.project_labels",
    description: "How the project labels in the vertical tabs panel look: the palette, per-project hues, sizes, and the colors on dark and light themes.",
);

#[cfg(test)]
#[path = "project_labels_tests.rs"]
mod tests;
