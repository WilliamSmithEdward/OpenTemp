//! Time-series area/line chart drawn with the egui painter.
//!
//! One y-axis per chart (never dual-axis), recessive grid, 2 px lines, a soft
//! area fill for single series, and a crosshair + tooltip on hover.

use egui::{Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use super::theme::palette;
use crate::history::Series;

pub struct Line<'a> {
    pub name: &'a str,
    pub series: Option<&'a Series>,
    pub color: Color32,
}

pub enum Scale {
    /// Fixed range, e.g. 0..100 for percentages.
    Fixed(f64, f64),
    /// From zero to a "nice" value above the visible maximum.
    AutoFromZero { min_span: f64 },
    /// Tight around the data (temperatures, clocks).
    AutoPadded { min_span: f64 },
}

pub struct Chart<'a> {
    pub lines: Vec<Line<'a>>,
    pub scale: Scale,
    pub height: f32,
    pub window_secs: f64,
    pub now: f64,
    pub fmt: &'a dyn Fn(f64) -> String,
    pub fill: bool,
}

fn nice_ceiling(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(v.log10().floor());
    let n = v / mag;
    let step = if n <= 1.0 {
        1.0
    } else if n <= 2.0 {
        2.0
    } else if n <= 2.5 {
        2.5
    } else if n <= 5.0 {
        5.0
    } else {
        10.0
    };
    step * mag
}

impl Chart<'_> {
    pub fn show(self, ui: &mut Ui) -> egui::Response {
        let p = palette(ui);
        let width = ui.available_width().max(120.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(width, self.height), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let painter = ui.painter_at(rect.expand(2.0));
        let label_font = FontId::proportional(11.0);
        let axis_w = 54.0;
        let plot = Rect::from_min_max(rect.min + vec2(0.0, 6.0), rect.max - vec2(axis_w, 18.0));

        // Visible range of every series.
        let t0 = self.now - self.window_secs;
        let mut vmin = f64::INFINITY;
        let mut vmax = f64::NEG_INFINITY;
        for l in &self.lines {
            if let Some(s) = l.series {
                for &(t, v) in s.points.iter().filter(|p| p.0 >= t0) {
                    let _ = t;
                    vmin = vmin.min(v as f64);
                    vmax = vmax.max(v as f64);
                }
            }
        }
        let has_data = vmin.is_finite();
        let (lo, hi) = match self.scale {
            Scale::Fixed(a, b) => (a, b),
            Scale::AutoFromZero { min_span } => {
                (0.0, nice_ceiling(if has_data { vmax.max(min_span) } else { min_span }))
            }
            Scale::AutoPadded { min_span } => {
                if !has_data {
                    (0.0, min_span)
                } else {
                    let mid = (vmin + vmax) / 2.0;
                    let span = (vmax - vmin).max(min_span) * 1.25;
                    let lo = (mid - span / 2.0).max(0.0);
                    (lo, lo + span)
                }
            }
        };
        let span = (hi - lo).max(1e-9);
        let to_screen = |t: f64, v: f64| -> Pos2 {
            let x = plot.left() + ((t - t0) / self.window_secs) as f32 * plot.width();
            let y = plot.bottom() - ((v - lo) / span) as f32 * plot.height();
            pos2(x, y.clamp(plot.top(), plot.bottom()))
        };

        // Grid: 4 horizontal hairlines with right-hand labels.
        for i in 0..=4 {
            let v = lo + span * i as f64 / 4.0;
            let y = plot.bottom() - plot.height() * i as f32 / 4.0;
            let stroke = if i == 0 { Stroke::new(1.0, p.baseline) } else { Stroke::new(1.0, p.grid) };
            painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], stroke);
            painter.text(
                pos2(plot.right() + 6.0, y),
                Align2::LEFT_CENTER,
                (self.fmt)(v),
                label_font.clone(),
                p.text_muted,
            );
        }
        // Time labels.
        painter.text(
            plot.left_bottom() + vec2(0.0, 4.0),
            Align2::LEFT_TOP,
            time_ago(self.window_secs),
            label_font.clone(),
            p.text_muted,
        );
        painter.text(plot.right_bottom() + vec2(0.0, 4.0), Align2::RIGHT_TOP, "now", label_font.clone(), p.text_muted);

        if !has_data {
            painter.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "Collecting data…",
                FontId::proportional(12.5),
                p.text_muted,
            );
            return resp;
        }

        // Series.
        for l in &self.lines {
            let Some(s) = l.series else { continue };
            let pts: Vec<Pos2> =
                s.points.iter().filter(|p| p.0 >= t0 - 1.0).map(|&(t, v)| to_screen(t, v as f64)).collect();
            if pts.len() < 2 {
                if let Some(&pt) = pts.first() {
                    painter.circle_filled(pt, 2.5, l.color);
                }
                continue;
            }
            if self.fill && self.lines.len() == 1 {
                let mut mesh = Mesh::default();
                let top = l.color.gamma_multiply(0.28);
                let bottom = l.color.gamma_multiply(0.03);
                for (i, pt) in pts.iter().enumerate() {
                    mesh.colored_vertex(*pt, top);
                    mesh.colored_vertex(pos2(pt.x, plot.bottom()), bottom);
                    if i > 0 {
                        let b = (i * 2) as u32;
                        mesh.add_triangle(b - 2, b - 1, b);
                        mesh.add_triangle(b - 1, b + 1, b);
                    }
                }
                painter.add(Shape::mesh(mesh));
            }
            painter.add(Shape::line(pts, Stroke::new(2.0, l.color)));
        }

        // Hover: crosshair, markers and a tooltip with every series' value.
        if let Some(pos) = resp.hover_pos().filter(|p| plot.expand2(vec2(0.0, 8.0)).contains(*p)) {
            let t = t0 + ((pos.x - plot.left()) / plot.width()) as f64 * self.window_secs;
            painter.line_segment([pos2(pos.x, plot.top()), pos2(pos.x, plot.bottom())], Stroke::new(1.0, p.baseline));
            let mut rows = Vec::new();
            for l in &self.lines {
                let Some(s) = l.series else { continue };
                if let Some(&(pt, pv)) = s.points.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())) {
                    let sp = to_screen(pt, pv as f64);
                    painter.circle_filled(sp, 4.5, p.surface);
                    painter.circle_filled(sp, 3.5, l.color);
                    rows.push((l.name, l.color, (self.fmt)(pv as f64), pt));
                }
            }
            if let Some(first) = rows.first() {
                let ago = (self.now - first.3).max(0.0);
                resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.label(
                        egui::RichText::new(if ago < 1.5 {
                            "now".to_string()
                        } else {
                            format!("{} ago", short_secs(ago))
                        })
                        .small()
                        .color(p.text_muted),
                    );
                    for (name, color, value, _) in &rows {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                            ui.painter().rect_filled(r, 2.0, *color);
                            ui.label(egui::RichText::new(*name).color(p.text_secondary));
                            ui.label(egui::RichText::new(value).strong().color(p.text));
                        });
                    }
                });
            }
        }
        resp
    }
}

fn short_secs(s: f64) -> String {
    let s = s.round() as u64;
    if s >= 60 { format!("{}m {:02}s", s / 60, s % 60) } else { format!("{s}s") }
}

fn time_ago(window: f64) -> String {
    if window >= 60.0 { format!("{} min ago", (window / 60.0).round()) } else { format!("{window:.0} s ago") }
}

/// A compact sparkline for cards and table rows (no axes, no hover).
pub fn sparkline(
    ui: &mut Ui,
    series: Option<&Series>,
    size: Vec2,
    color: Color32,
    range: Option<(f64, f64)>,
    window: f64,
    now: f64,
) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let Some(s) = series else { return };
    let t0 = now - window;
    let pts: Vec<(f64, f32)> = s.points.iter().filter(|p| p.0 >= t0).copied().collect();
    if pts.len() < 2 {
        return;
    }
    let (lo, hi) = range.unwrap_or_else(|| {
        let mn = pts.iter().map(|p| p.1 as f64).fold(f64::INFINITY, f64::min);
        let mx = pts.iter().map(|p| p.1 as f64).fold(f64::NEG_INFINITY, f64::max);
        let pad = ((mx - mn) * 0.15).max(0.5);
        ((mn - pad).max(0.0), mx + pad)
    });
    let span = (hi - lo).max(1e-9);
    let line: Vec<Pos2> = pts
        .iter()
        .map(|&(t, v)| {
            pos2(
                rect.left() + ((t - t0) / window) as f32 * rect.width(),
                (rect.bottom() - ((v as f64 - lo) / span) as f32 * rect.height()).clamp(rect.top(), rect.bottom()),
            )
        })
        .collect();
    let painter = ui.painter_at(rect.expand(1.0));
    let mut mesh = Mesh::default();
    for (i, pt) in line.iter().enumerate() {
        mesh.colored_vertex(*pt, color.gamma_multiply(0.22));
        mesh.colored_vertex(pos2(pt.x, rect.bottom()), color.gamma_multiply(0.02));
        if i > 0 {
            let b = (i * 2) as u32;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.add(Shape::line(line, Stroke::new(1.5, color)));
}

#[cfg(test)]
mod tests {
    #[test]
    fn nice_ceiling_rounds_up() {
        assert_eq!(super::nice_ceiling(73.0), 100.0);
        assert_eq!(super::nice_ceiling(12.0), 20.0);
        assert_eq!(super::nice_ceiling(2.2), 2.5);
        assert_eq!(super::nice_ceiling(0.0), 1.0);
    }
}
