//! Browser-style tab bar rendering and hit testing.
//!
//! Draws one strip of filled tabs with rounded top corners, each with a close
//! button on the right, and a new-tab button at the end. Closed tabs wait at
//! the right edge, narrow and red, until their grace period ends. On macOS
//! the strip starts past the native traffic lights so the window looks like
//! a browser, tabs beside the window controls.

use alacritty_terminal::index::{Column, Point};
use unicode_width::UnicodeWidthChar;

use crate::config::UiConfig;
use crate::config::tabs::TabBarEdge;
use crate::display::color::Rgb;
use crate::renderer::rects::RenderRect;
use crate::string::{ShortenDirection, StrShortener};
use crate::updater::UpdateState;

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
/// Width of a closed tab, in cells. Its title is cut to fit beside the
/// restore button.
const CLOSED_TAB_CELLS: usize = 10;
/// Floor a closed tab shrinks to when the strip overflows, the restore
/// button alone.
const CLOSED_TAB_SHRINK: usize = 4;
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

/// Width of each closed tab, in cells. `available` is the strip room left
/// for tabs, `open_cells` what the open tabs need at their natural width.
fn closed_tab_span(open_cells: usize, closed_count: usize, available: usize) -> usize {
    if closed_count == 0 {
        return 0;
    }
    let spare = available.saturating_sub(open_cells) / closed_count;
    spare.clamp(CLOSED_TAB_SHRINK, CLOSED_TAB_CELLS)
}

impl Display {
    pub(super) fn draw_tab_bar(
        &mut self,
        config: &UiConfig,
        entries: &[TabEntry],
        update: &UpdateState,
        line: usize,
    ) {
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
        let controls_cells = if num_cols > WINDOW_BTN_CELLS * 3 { WINDOW_BTN_CELLS * 3 } else { 0 };
        #[cfg(target_os = "macos")]
        let controls_cells = 0;

        // The update chip sits left of the controls. It only shows when a tab
        // and the new-tab button still fit beside it.
        let chip_label = update.label().map(|label| format!(" {label} "));
        let chip_cells = chip_label.as_deref().map_or(0, label_cells);
        let chip_fits = chip_cells > 0
            && num_cols > controls_cells + chip_cells + TAB_GAP + MIN_TAB_CELLS + NEW_TAB_CELLS + 1;
        let chip_col = chip_fits.then(|| num_cols - controls_cells - chip_cells - TAB_GAP);
        let right_reserved = controls_cells + if chip_fits { chip_cells + TAB_GAP } else { 0 };
        let tab_area_cols = num_cols.saturating_sub(right_reserved);

        // Natural width per tab, as wide as its full title needs. The only
        // title cap is the user's tab_title_max_length, 0 means unlimited.
        // A closed tab has a fixed narrow width instead, its title cut to fit.
        let max_title = config.tabs.tab_title_max_length;
        let mut open: Vec<(usize, bool, String, usize)> = Vec::new();
        let mut closed: Vec<(usize, String)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match entry {
                TabEntry::Open { title, active } => {
                    let title: String = if max_title > 0 {
                        StrShortener::new(
                            title,
                            max_title,
                            ShortenDirection::Right,
                            Some(SHORTENER),
                        )
                        .collect()
                    } else {
                        title.clone()
                    };
                    let label = format!(" {title}");
                    let span = (label_cells(&label) + CLOSE_CELLS).max(MIN_TAB_CELLS);
                    open.push((index, *active, label, span));
                },
                TabEntry::Closed { title } => closed.push((index, format!(" {title}"))),
            }
        }

        // Closed tabs pack at the right edge of the strip, a gap away from
        // the new-tab button. Open tabs keep their width first, so the closed
        // ones shrink to their floor before any open tab gives up a cell.
        let open_gaps = TAB_GAP * open.len().saturating_sub(1);
        let closed_gaps = TAB_GAP * closed.len().saturating_sub(1);
        let group_gap = if closed.is_empty() { 0 } else { TAB_GAP };
        let available =
            tab_area_cols.saturating_sub(start_col + NEW_TAB_CELLS + 1 + group_gap + closed_gaps);
        let open_total: usize = open.iter().map(|tab| tab.3).sum();
        let closed_span = closed_tab_span(open_total + open_gaps, closed.len(), available);
        let closed_width = closed.len() * closed_span + closed_gaps;
        let open_area_cols = tab_area_cols.saturating_sub(closed_width + group_gap);

        // When the open tabs still overflow, cap the widest first so short
        // tabs keep their natural width, like a browser. Binary search the
        // largest cap that still fits the strip, down to a floor.
        let budget = available.saturating_sub(open_gaps + closed.len() * closed_span);
        let cap = (!open.is_empty() && open_total > budget).then(|| {
            let mut lo = MIN_TAB_SHRINK;
            let mut hi = open.iter().map(|tab| tab.3).max().unwrap_or(lo).max(lo);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let capped: usize = open.iter().map(|tab| tab.3.min(mid)).sum();
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
        for (index, active, mut label, natural_span) in open {
            let span = cap.map_or(natural_span, |cap| natural_span.min(cap));
            if column + span >= open_area_cols {
                break;
            }
            if span < natural_span {
                // Refit the title to the narrower tab.
                let fit = span.saturating_sub(CLOSE_CELLS);
                label = StrShortener::new(&label, fit, ShortenDirection::Right, Some(SHORTENER))
                    .collect();
            }
            layouts.push(TabLayout { index, start: column, span, label, active, closed: false });
            column += span + TAB_GAP;
        }
        let new_tab_col = column;
        let show_new_tab = new_tab_col + NEW_TAB_CELLS < open_area_cols;

        let mut column = tab_area_cols.saturating_sub(closed_width);
        for (index, label) in closed {
            // At the floor only the restore button is left, so no title.
            let fit = closed_span - CLOSE_CELLS;
            let label = if fit > 1 {
                StrShortener::new(&label, fit, ShortenDirection::Right, Some(SHORTENER)).collect()
            } else {
                String::new()
            };
            layouts.push(TabLayout {
                index,
                start: column,
                span: closed_span,
                label,
                active: false,
                closed: true,
            });
            column += closed_span + TAB_GAP;
        }

        // A drag past the start threshold floats the pressed tab under the
        // pointer, leaving an empty slot where it would land.
        let strip_left = pad_x + cw * start_col as f32;
        let strip_right = pad_x + cw * open_area_cols as f32;
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
        let closed_hover_bg = Rgb::new(0xd4, 0x4a, 0x4a);
        let closed_bg = blend_rgb(inactive_bg, closed_hover_bg, 0.6);
        let closed_fg = Rgb::new(0xf6, 0xf6, 0xf6);

        // Rect pass: bar background, rounded tab backgrounds, hit boxes.
        let mut rects =
            vec![RenderRect::new(0., y as f32, bar_width as f32, bar_height as f32, bar_bg, alpha)];
        for layout in &layouts {
            let tab_hovered = hovered == Some(TabHit::Select(layout.index))
                || hovered == Some(TabHit::Close(layout.index));
            let tab_bg = if layout.closed {
                if hovered == Some(TabHit::Restore(layout.index)) {
                    closed_hover_bg
                } else {
                    closed_bg
                }
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
                    closed_hover_bg
                } else {
                    closed_bg
                };
                let rendered_bg = blend_rgb(base_bg, bg, alpha);
                self.draw_tab_bar_text(
                    Point::new(line, Column(layout.start + centering_offset(layout))),
                    closed_fg,
                    rendered_bg,
                    alpha,
                    &layout.label,
                    &size_info,
                );
                self.draw_tab_bar_text(
                    Point::new(line, Column(layout.start + layout.span - 2)),
                    closed_fg,
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

        // The update chip, a pill in green while a release is offered or
        // installed, muted while it downloads, red when it failed.
        if let (Some(label), Some(col)) = (&chip_label, chip_col) {
            let green = Rgb::new(0x10, 0xb9, 0x81);
            let chip_hovered = hovered == Some(TabHit::Update);
            let (fg, chip_bg) = match update {
                UpdateState::Available(_) | UpdateState::Restart(_) => {
                    let tint = if chip_hovered { 0.35 } else { 0.18 };
                    (green, blend_rgb(bar_bg, green, tint))
                },
                UpdateState::Failed => (close_hover_fg, blend_rgb(bar_bg, close_hover_fg, 0.18)),
                _ => (inactive_fg, blend_rgb(bar_bg, active_bg, 0.5)),
            };
            let x = pad_x + cw * col as f32;
            let w = cw * chip_cells as f32;
            let rendered_bg = blend_rgb(base_bg, chip_bg, alpha);
            let mut rects = vec![RenderRect::new(x, tab_top, w, tab_height, rendered_bg, alpha)];
            // All four corners are cut with the strip color, so it reads as a pill.
            for row in 0..radius {
                let cut = (radius - row) as f32;
                let top = tab_top + row as f32;
                let bottom = tab_top + tab_height - 1.0 - row as f32;
                for ry in [top, bottom] {
                    rects.push(RenderRect::new(x, ry, cut, 1.0, bar_bg, alpha));
                    rects.push(RenderRect::new(x + w - cut, ry, cut, 1.0, bar_bg, alpha));
                }
            }
            self.renderer.draw_rects(&size_info, &metrics, rects);
            self.draw_tab_bar_text(
                Point::new(line, Column(col)),
                fg,
                rendered_bg,
                alpha,
                label,
                &size_info,
            );
            if update.clickable() {
                self.tab_hit_boxes.push(TabHitBox {
                    hit: TabHit::Update,
                    x: x as i32,
                    y,
                    width: w as i32,
                    height: bar_height,
                });
            }
        }

        // Window controls on the right and a drag region over the whole strip.
        // Only on platforms where we removed the native title bar.
        #[cfg(not(target_os = "macos"))]
        if controls_cells > 0 {
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

#[cfg(test)]
mod tests {
    use super::{CLOSED_TAB_CELLS, CLOSED_TAB_SHRINK, closed_tab_span};

    #[test]
    fn closed_tabs_shrink_before_open_tabs_and_stop_at_the_floor() {
        assert_eq!(closed_tab_span(20, 0, 40), 0);
        assert_eq!(closed_tab_span(20, 1, 40), CLOSED_TAB_CELLS);
        assert_eq!(closed_tab_span(34, 1, 40), 6);
        assert_eq!(closed_tab_span(40, 2, 40), CLOSED_TAB_SHRINK);
    }
}
