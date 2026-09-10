//! Frame drawn along the window edges on Windows.
//!
//! The native title bar is removed there, so two windows on the same theme
//! blend into each other where they overlap. A thin line at the edges keeps
//! the outline visible. A maximized or fullscreen window has no neighbour to
//! blend into and skips the frame, like a browser.

use crate::config::UiConfig;
use crate::config::window::Decorations;
use crate::renderer::rects::RenderRect;

use super::{Display, blend_rgb};

impl Display {
    pub(super) fn draw_window_border(&mut self, config: &UiConfig) {
        if config.window.decorations == Decorations::Full
            || self.window.is_maximized()
            || self.window.is_fullscreen()
        {
            return;
        }

        let size_info = self.size_info;
        let width = size_info.width();
        let height = size_info.height();
        let thickness = (self.window.scale_factor as f32).round().max(1.0);
        let color = config.window.border_color.unwrap_or_else(|| {
            blend_rgb(config.colors.primary.background, config.colors.primary.foreground, 0.4)
        });

        let edges = [
            (0.0, 0.0, width, thickness),
            (0.0, height - thickness, width, thickness),
            (0.0, 0.0, thickness, height),
            (width - thickness, 0.0, thickness, height),
        ];
        for (x, y, w, h) in edges {
            let (x, y, w, h) = (x as i32, y as i32, w as i32, h as i32);
            self.damage_tracker.frame().add_viewport_rect(&size_info, x, y, w, h);
            self.damage_tracker.next_frame().add_viewport_rect(&size_info, x, y, w, h);
        }

        let rects =
            edges.map(|(x, y, w, h)| RenderRect::new(x, y, w, h, color, 1.0)).into_iter().collect();
        let metrics = self.glyph_cache.font_metrics();
        self.renderer.draw_rects(&size_info, &metrics, rects);
    }
}
