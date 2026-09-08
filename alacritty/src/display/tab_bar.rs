//! Browser-style tab bar rendering and hit testing.
//!
//! Draws one strip of filled tabs with rounded top corners, each with a close
//! button on the right, and a new-tab button at the end. On macOS the strip
//! starts past the native traffic lights so the window looks like a browser,
//! tabs beside the window controls.

use alacritty_terminal::index::{Column, Point};
use unicode_width::UnicodeWidthChar;

use crate::config::UiConfig;
use crate::config::tabs::TabBarEdge;
use crate::display::color::Rgb;
use crate::renderer::rects::RenderRect;
use crate::string::{ShortenDirection, StrShortener};

use super::{
    Display, SHORTENER, TabEntry, TabHit, TabHitBox, blend_rgb, darken_rgb,
    tab_bar_background_alpha,
};

/// Smallest tab width, in cells, so short titles still look even.
const MIN_TAB_CELLS: usize = 12;
/// Floor a tab shrinks to when the strip overflows, in cells.
const MIN_TAB_SHRINK: usize = 7;
/// Cells reserved on the right of a tab for the close button.
const CLOSE_CELLS: usize = 3;
/// Cells for the new-tab button, " + ".
const NEW_TAB_CELLS: usize = 3;
/// Empty cells between two tabs.
const TAB_GAP: usize = 1;
/// Width kept clear on the left for the macOS traffic lights, in points.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_POINTS: f32 = 78.0;
/// Width of each window control button, minimize, maximize, close, in cells.
/// On Windows and Linux we draw our own controls at the right of the strip,
/// like a browser, since the native title bar is removed.
#[cfg(not(target_os = "macos"))]
const WINDOW_BTN_CELLS: usize = 4;

/// One laid-out tab, shared between the rect pass and the text pass.
struct TabLayout {
    index: usize,
    start: usize,
    span: usize,
    label: String,
    active: bool,
    closed: bool,
}

/// Width of a label in terminal cells.
fn label_cells(label: &str) -> usize {
    label.chars().map(|c| c.width().unwrap_or(1)).sum()
}

/// Start offset that centers a label in the tab's text area. Zero for a tab
/// at its natural width, only labels shorter than the minimum tab width move.
fn centering_offset(layout: &TabLayout) -> usize {
    (layout.span - CLOSE_CELLS).saturating_sub(label_cells(&layout.label)) / 2
}

impl Display {
    pub(super) fn draw_tab_bar(&mut self, config: &UiConfig, entries: &[TabEntry], line: usize) {
        let ch = self.size_info.cell_height();
        let band_height = ch * config.tabs.tab_bar_height.as_f32();
        let text_inset = (band_height - ch) / 2.0;

        let mut size_info = self.size_info;
        // Center the single line of tab text vertically in the taller band.
        if config.tabs.tab_bar_edge == TabBarEdge::Top {
            size_info.padding_y -= band_height - text_inset;
        }
        self.renderer.set_viewport(&size_info);

        let metrics = self.glyph_cache.font_metrics();
        let cw = size_info.cell_width();
        let num_cols = size_info.columns;
        let pad_x = size_info.padding_x();

        let text_y = ch * line as f32 + size_info.padding_y();
        let y = (text_y - text_inset) as i32;
        let bar_width = size_info.width() as i32;
        let bar_height = band_height as i32;

        // Tabs sit slightly inset from the strip edges, like browser tabs.
        let tab_pad = ch * 0.16;
        let tab_top = text_y - text_inset + tab_pad;
        let tab_height = band_height - 2.0 * tab_pad;
        let radius = (ch * 0.28).round().max(2.0) as i32;

        self.damage_tracker.frame().add_viewport_rect(&size_info, 0, y, bar_width, bar_height);
        self.damage_tracker.next_frame().add_viewport_rect(&size_info, 0, y, bar_width, bar_height);

        let base_bg = config.colors.primary.background;
        let opacity = config.window_opacity();
        let alpha = tab_bar_background_alpha(opacity);
        let bar_bg = config.tabs.tab_bar_background.unwrap_or(base_bg * 0.8);
        let active_fg =
            config.tabs.active_tab_foreground.unwrap_or(config.colors.footer_bar_foreground());
        let active_bg =
            config.tabs.active_tab_background.unwrap_or(config.colors.footer_bar_background());
        let inactive_fg =
            config.tabs.inactive_tab_foreground.unwrap_or(config.colors.primary.foreground);
        let inactive_bg = config.tabs.inactive_tab_background.unwrap_or(darken_rgb(bar_bg, 0.82));

        // On macOS the strip shares the row with the traffic lights, so start
        // the first tab past them. Fullscreen hides the lights, so the first
        // tab sits flush left like a browser.
        #[cfg(target_os = "macos")]
        let start_col = if self.window.is_fullscreen() {
            0
        } else {
            let inset = TRAFFIC_LIGHT_POINTS * self.window.scale_factor as f32;
            ((inset / cw).ceil() as usize).max(1)
        };
        #[cfg(not(target_os = "macos"))]
        let start_col = 0;

        // Cells kept clear on the right for our own window controls.
        #[cfg(not(target_os = "macos"))]
        let right_reserved = if num_cols > WINDOW_BTN_CELLS * 3 { WINDOW_BTN_CELLS * 3 } else { 0 };
        #[cfg(target_os = "macos")]
        let right_reserved = 0;
        let tab_area_cols = num_cols.saturating_sub(right_reserved);

        // Natural width per tab, as wide as its full title needs. The only
        // title cap is the user's tab_title_max_length, 0 means unlimited.
        let max_title = config.tabs.tab_title_max_length;
        let mut tabs: Vec<(usize, bool, bool, String, usize)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let (title, active, closed) = match entry {
                TabEntry::Open { title, active } => (title, *active, false),
                TabEntry::Closed { title } => (title, false, true),
            };
            let title: String = if max_title > 0 {
                StrShortener::new(title, max_title, ShortenDirection::Right, Some(SHORTENER))
                    .collect()
            } else {
                title.clone()
            };
            let label = format!(" {title}");
            let span = (label_cells(&label) + CLOSE_CELLS).max(MIN_TAB_CELLS);
            tabs.push((index, active, closed, label, span));
        }

        // When the tabs overflow the strip, cap the widest tabs first so
        // short tabs keep their natural width, like a browser. Binary search
        // the largest cap that still fits the strip, down to a floor.
        let tab_count = tabs.len();
        let gaps = TAB_GAP * tab_count.saturating_sub(1);
        let available = tab_area_cols.saturating_sub(start_col + NEW_TAB_CELLS + 1);
        let budget = available.saturating_sub(gaps);
        let total: usize = tabs.iter().map(|tab| tab.4).sum();
        let cap = (tab_count > 0 && total > budget).then(|| {
            let mut lo = MIN_TAB_SHRINK;
            let mut hi = tabs.iter().map(|tab| tab.4).max().unwrap_or(lo).max(lo);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let capped: usize = tabs.iter().map(|tab| tab.4.min(mid)).sum();
                if capped <= budget {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            lo
        });

        // Lay tabs out first so the rects and the text agree on positions.
        let mut layouts: Vec<TabLayout> = Vec::new();
        let mut column = start_col;
        for (index, active, closed, mut label, natural_span) in tabs {
            let span = cap.map_or(natural_span, |cap| natural_span.min(cap));
            if column + span >= tab_area_cols {
                break;
            }
            if span < natural_span {
                // Refit the title to the narrower tab.
                let fit = span.saturating_sub(CLOSE_CELLS);
                label = StrShortener::new(&label, fit, ShortenDirection::Right, Some(SHORTENER))
                    .collect();
            }
            layouts.push(TabLayout { index, start: column, span, label, active, closed });
            column += span + TAB_GAP;
        }
        let new_tab_col = column;
        let show_new_tab = new_tab_col + NEW_TAB_CELLS < tab_area_cols;

        // A drag past the start threshold floats the pressed tab under the
        // pointer, leaving an empty slot where it would land.
        let strip_left = pad_x + cw * start_col as f32;
        let strip_right = pad_x + cw * tab_area_cols as f32;
        let floating = self.tab_drag.as_ref().filter(|drag| drag.active).and_then(|drag| {
            let layout = layouts.iter().find(|layout| layout.index == drag.index)?;
            let width = cw * layout.span as f32;
            let max_x = (strip_right - width).max(strip_left);
            let x = (drag.pointer_x - drag.grab_dx.clamp(0., width)).clamp(strip_left, max_x);
            Some((drag.index, x))
        });

        let hovered = if floating.is_some() { None } else { self.hovered_tab };
        let hover_bg = blend_rgb(inactive_bg, active_bg, 0.5);
        let close_hover_fg = Rgb::new(0xec, 0x5f, 0x5f);
        let stub_hover_bg = Rgb::new(0xd4, 0x4a, 0x4a);
        let stub_bg = blend_rgb(inactive_bg, stub_hover_bg, 0.6);
        let stub_fg = Rgb::new(0xf6, 0xf6, 0xf6);

        // Rect pass: bar background, rounded tab backgrounds, hit boxes.
        let mut rects =
            vec![RenderRect::new(0., y as f32, bar_width as f32, bar_height as f32, bar_bg, alpha)];
        for layout in &layouts {
            let tab_hovered = hovered == Some(TabHit::Select(layout.index))
                || hovered == Some(TabHit::Close(layout.index));
            let tab_bg = if layout.closed {
                if hovered == Some(TabHit::Restore(layout.index)) { stub_hover_bg } else { stub_bg }
            } else if layout.active {
                active_bg
            } else if tab_hovered {
                hover_bg
            } else {
                inactive_bg
            };
            let (rendered_bg, bg_alpha) = if layout.active {
                (tab_bg, 1.0)
            } else {
                (blend_rgb(base_bg, tab_bg, alpha), alpha)
            };
            let x = pad_x + cw * layout.start as f32;
            let w = cw * layout.span as f32;
            // The floating tab keeps its slot hit boxes for the reorder math,
            // but its slot stays empty, the tab itself is drawn last on top.
            let in_slot = floating.is_none_or(|(index, _)| index != layout.index);
            if in_slot {
                rects.push(RenderRect::new(x, tab_top, w, tab_height, rendered_bg, bg_alpha));
                // Round the top corners by cutting them with the strip color.
                for row in 0..radius {
                    let cut = (radius - row) as f32;
                    let ry = tab_top + row as f32;
                    rects.push(RenderRect::new(x, ry, cut, 1.0, bar_bg, alpha));
                    rects.push(RenderRect::new(x + w - cut, ry, cut, 1.0, bar_bg, alpha));
                }
            }

            if layout.closed {
                self.tab_hit_boxes.push(TabHitBox {
                    hit: TabHit::Restore(layout.index),
                    x: x as i32,
                    y,
                    width: w as i32,
                    height: bar_height,
                });
                continue;
            }

            let close_start = layout.start + layout.span - CLOSE_CELLS;
            self.tab_hit_boxes.push(TabHitBox {
                hit: TabHit::Select(layout.index),
                x: x as i32,
                y,
                width: (cw * (layout.span - CLOSE_CELLS) as f32) as i32,
                height: bar_height,
            });
            self.tab_hit_boxes.push(TabHitBox {
                hit: TabHit::Close(layout.index),
                x: (pad_x + cw * close_start as f32) as i32,
                y,
                width: (cw * CLOSE_CELLS as f32) as i32,
                height: bar_height,
            });
        }
        if show_new_tab {
            self.tab_hit_boxes.push(TabHitBox {
                hit: TabHit::New,
                x: (pad_x + cw * new_tab_col as f32) as i32,
                y,
                width: (cw * NEW_TAB_CELLS as f32) as i32,
                height: bar_height,
            });
        }
        self.renderer.draw_rects(&size_info, &metrics, rects);

        // Text pass: labels, close buttons, new-tab button.
        for layout in &layouts {
            if floating.is_some_and(|(index, _)| index == layout.index) {
                continue;
            }
            if layout.closed {
                let bg = if hovered == Some(TabHit::Restore(layout.index)) {
                    stub_hover_bg
                } else {
                    stub_bg
                };
                let rendered_bg = blend_rgb(base_bg, bg, alpha);
                self.draw_tab_bar_text(
                    Point::new(line, Column(layout.start + centering_offset(layout))),
                    stub_fg,
                    rendered_bg,
                    alpha,
                    &layout.label,
                    &size_info,
                );
                self.draw_tab_bar_text(
                    Point::new(line, Column(layout.start + layout.span - 2)),
                    stub_fg,
                    rendered_bg,
                    alpha,
                    "\u{21ba}",
                    &size_info,
                );
                continue;
            }
            let tab_hovered = hovered == Some(TabHit::Select(layout.index))
                || hovered == Some(TabHit::Close(layout.index));
            let close_hovered = hovered == Some(TabHit::Close(layout.index));
            let (fg, tab_bg) = if layout.active {
                (active_fg, active_bg)
            } else if tab_hovered {
                (inactive_fg, hover_bg)
            } else {
                (inactive_fg, inactive_bg)
            };
            let (rendered_bg, bg_alpha) = if layout.active {
                (tab_bg, 1.0)
            } else {
                (blend_rgb(base_bg, tab_bg, alpha), alpha)
            };
            self.draw_tab_bar_text(
                Point::new(line, Column(layout.start + centering_offset(layout))),
                fg,
                rendered_bg,
                bg_alpha,
                &layout.label,
                &size_info,
            );
            let close_fg = if close_hovered { close_hover_fg } else { fg };
            self.draw_tab_bar_text(
                Point::new(line, Column(layout.start + layout.span - 2)),
                close_fg,
                rendered_bg,
                bg_alpha,
                "\u{00d7}",
                &size_info,
            );
        }
        if show_new_tab {
            self.draw_tab_bar_text(
                Point::new(line, Column(new_tab_col)),
                inactive_fg,
                bar_bg,
                alpha,
                " + ",
                &size_info,
            );
        }

        // Window controls on the right and a drag region over the whole strip.
        // Only on platforms where we removed the native title bar.
        #[cfg(not(target_os = "macos"))]
        if right_reserved > 0 {
            let controls = [
                (num_cols - WINDOW_BTN_CELLS * 3, TabHit::Minimize, " \u{2013}  "),
                (num_cols - WINDOW_BTN_CELLS * 2, TabHit::MaximizeToggle, " \u{25a1}  "),
                (num_cols - WINDOW_BTN_CELLS, TabHit::WindowClose, " \u{00d7}  "),
            ];
            for (col, hit, glyph) in controls {
                self.tab_hit_boxes.push(TabHitBox {
                    hit,
                    x: (pad_x + cw * col as f32) as i32,
                    y,
                    width: (cw * WINDOW_BTN_CELLS as f32) as i32,
                    height: bar_height,
                });
                let (btn_fg, btn_bg, btn_alpha) = if hovered != Some(hit) {
                    (inactive_fg, bar_bg, alpha)
                } else if hit == TabHit::WindowClose {
                    (Rgb::new(0xff, 0xff, 0xff), Rgb::new(0xe8, 0x11, 0x23), 1.0)
                } else {
                    (inactive_fg, blend_rgb(bar_bg, active_bg, 0.5), 1.0)
                };
                self.draw_tab_bar_text(
                    Point::new(line, Column(col)),
                    btn_fg,
                    btn_bg,
                    btn_alpha,
                    glyph,
                    &size_info,
                );
            }
        }

        // The whole strip drags the window. Pushed last so tab and control hit
        // boxes above win over this drag region.
        self.tab_hit_boxes.push(TabHitBox {
            hit: TabHit::Caption,
            x: 0,
            y,
            width: bar_width,
            height: bar_height,
        });

        // The floating tab draws after everything else so it covers its
        // neighbors while it moves. Press always selects the tab, so it uses
        // the active style.
        if let Some((index, x)) = floating {
            if let Some(layout) = layouts.iter().find(|layout| layout.index == index) {
                let width = cw * layout.span as f32;
                let mut rects = vec![RenderRect::new(x, tab_top, width, tab_height, active_bg, 1.)];
                for row in 0..radius {
                    let cut = (radius - row) as f32;
                    let ry = tab_top + row as f32;
                    rects.push(RenderRect::new(x, ry, cut, 1.0, bar_bg, alpha));
                    rects.push(RenderRect::new(x + width - cut, ry, cut, 1.0, bar_bg, alpha));
                }
                self.renderer.draw_rects(&size_info, &metrics, rects);

                // A shifted viewport places the glyphs at the fractional
                // pixel offset that cell coordinates cannot express.
                self.renderer.set_shifted_viewport(&size_info, x - pad_x);
                self.draw_tab_bar_text(
                    Point::new(line, Column(centering_offset(layout))),
                    active_fg,
                    active_bg,
                    1.,
                    &layout.label,
                    &size_info,
                );
                self.draw_tab_bar_text(
                    Point::new(line, Column(layout.span - 2)),
                    active_fg,
                    active_bg,
                    1.,
                    "\u{00d7}",
                    &size_info,
                );
            }
        }

        self.renderer.set_viewport(&self.size_info);
    }
}
