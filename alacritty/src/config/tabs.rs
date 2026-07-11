use serde::{Deserialize, Deserializer, Serialize};

use alacritty_config_derive::{ConfigDeserialize, SerdeReplace};

use crate::display::color::Rgb;

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Tabs {
    pub tab_bar_edge: TabBarEdge,
    pub tab_bar_style: TabBarStyle,
    pub tab_powerline_style: TabPowerlineStyle,
    pub tab_bar_min_tabs: usize,
    pub tab_bar_height: TabBarHeight,
    pub tab_switch_strategy: TabSwitchStrategy,
    pub tab_separator: String,
    pub tab_title_template: String,
    pub active_tab_title_template: Option<String>,
    pub tab_title_max_length: usize,
    pub active_tab_foreground: Option<Rgb>,
    pub active_tab_background: Option<Rgb>,
    pub active_tab_font_style: TabFontStyle,
    pub inactive_tab_foreground: Option<Rgb>,
    pub inactive_tab_background: Option<Rgb>,
    pub inactive_tab_font_style: TabFontStyle,
    pub tab_bar_background: Option<Rgb>,
    pub mouse: TabMouse,
}

impl Tabs {
    #[inline]
    pub fn display_tab_bar(&self, tab_count: usize) -> bool {
        self.tab_bar_style != TabBarStyle::Hidden && tab_count >= self.tab_bar_min_tabs
    }
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            tab_bar_edge: Default::default(),
            tab_bar_style: Default::default(),
            tab_powerline_style: Default::default(),
            tab_bar_min_tabs: 1,
            tab_bar_height: Default::default(),
            tab_switch_strategy: Default::default(),
            tab_separator: String::from(" | "),
            tab_title_template: String::from("{index}: {title}"),
            active_tab_title_template: None,
            tab_title_max_length: 0,
            active_tab_foreground: None,
            active_tab_background: None,
            active_tab_font_style: Default::default(),
            inactive_tab_foreground: None,
            inactive_tab_background: None,
            inactive_tab_font_style: Default::default(),
            tab_bar_background: None,
            mouse: Default::default(),
        }
    }
}

/// Height of the tab bar, in cell heights. Below one cell the tab titles no
/// longer fit, so deserialization clamps the value to a sane range.
#[derive(SerdeReplace, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct TabBarHeight(f32);

impl Default for TabBarHeight {
    fn default() -> Self {
        TabBarHeight(2.4)
    }
}

impl TabBarHeight {
    pub fn as_f32(self) -> f32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for TabBarHeight {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(TabBarHeight(f32::deserialize(deserializer)?.clamp(1.0, 10.0)))
    }
}

#[derive(ConfigDeserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabBarEdge {
    #[default]
    Top,
    Bottom,
}

#[derive(ConfigDeserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabBarStyle {
    Hidden,
    Separator,
    Slant,
    #[default]
    Powerline,
    Fade,
}

#[derive(ConfigDeserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabPowerlineStyle {
    #[default]
    Angled,
    Slanted,
    Round,
}

#[derive(ConfigDeserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabSwitchStrategy {
    #[default]
    Previous,
    Left,
    Right,
    Last,
}

#[derive(ConfigDeserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TabFontStyle {
    #[default]
    Normal,
    Bold,
    Italic,
    BoldItalic,
}

#[derive(ConfigDeserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMouse {
    pub enabled: bool,
    pub hover: bool,
}

impl Default for TabMouse {
    fn default() -> Self {
        Self { enabled: true, hover: true }
    }
}
