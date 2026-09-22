//! A resize grip drawn in the bottom-right corner on Windows.
//!
//! The native frame is removed there, so the only resize handle is the thin
//! edge the app detects itself. A filled triangle in the corner marks where to
//! grab and pairs with the enlarged grab zone in the input handler. Hidden
//! while maximized or fullscreen, like the window border, since neither resizes.

use crate::config::UiConfig;
use crate::config::window::Decorations;
use crate::renderer::rects::RenderTriangle;

use super::{Display, blend_rgb};

impl Display {
    pub(super) fn draw_resize_grip(&mut self, config: &UiConfig) {
        if config.window.decorations == Decorations::Full
            || self.window.is_maximized()
            || self.window.is_fullscreen()
        {
            return;
        }

        let width = self.size_info.width();
        let height = self.size_info.height();
        // Physical pixels, matching the grab zone in the input handler. The grip
        // reads as a hint, so it is a bit smaller than the zone.
        let size = (config.window.resize_corner_size as f32 * 0.6).round().max(6.0);
        let color = config.window.border_color.unwrap_or_else(|| {
            blend_rgb(config.colors.primary.background, config.colors.primary.foreground, 0.4)
        });

        let grip = RenderTriangle {
            points: [(width, height - size), (width, height), (width - size, height)],
            color,
            alpha: 1.0,
        };
        self.renderer.draw_triangles(&self.size_info, &[grip]);
    }
}
