//! Reusable building blocks: cards, stat tiles, meters, status pills, banners
//! and the vector icon set (drawn, so they stay crisp at any DPI).

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Response, RichText, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use super::theme::{Level, Palette, palette};

pub fn card_frame(p: &Palette) -> Frame {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    card_frame(p)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A card that acts as a link to a detail page: hover highlight, pointer cursor,
/// keyboard focusable, and announced as a button to screen readers.
pub fn link_card(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    add: impl FnOnce(&mut Ui),
) -> Response {
    let p = palette(ui);
    let id = ui.scope_id().with(id);
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered() || r.has_focus());
    let frame = card_frame(p).stroke(Stroke::new(1.0, if hovered { p.baseline } else { p.border }));
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
    let resp = ui.interact(inner.response.rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let label = label.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    if resp.has_focus() {
        ui.painter().rect_stroke(inner.response.rect, 10.0, Stroke::new(2.0, p.accent), StrokeKind::Outside);
    }
    resp
}

pub fn card_header(ui: &mut Ui, icon: Icon, title: &str, trailing: Option<&str>) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        icon_label(ui, icon, 16.0, p.text_secondary);
        ui.label(RichText::new(title).strong().size(14.0).color(p.text));
        if let Some(t) = trailing {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(t).small().color(p.text_muted)).truncate());
            });
        }
    });
    ui.add_space(6.0);
}

pub fn section_title(ui: &mut Ui, title: &str) {
    let p = palette(ui);
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong().size(15.0).color(p.text));
    ui.add_space(4.0);
}

/// A labelled value. `level` adds a coloured dot *and* a text label so the
/// meaning never relies on colour alone.
pub fn stat(ui: &mut Ui, label: &str, value: &str, level: Option<Level>) {
    let p = palette(ui);
    ui.vertical(|ui| {
        ui.label(RichText::new(label).small().color(p.text_muted));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(value).size(18.0).strong().color(p.text));
            if let Some(l) = level.filter(|l| *l != Level::Normal) {
                level_pill(ui, l);
            }
        });
    });
}

/// A row of stat tiles that wraps on narrow windows.
pub fn stat_row(ui: &mut Ui, items: &[(&str, String, Option<Level>)]) {
    let min_w = 150.0;
    let avail = ui.available_width();
    let per_row = ((avail / min_w).floor() as usize).clamp(1, items.len().max(1));
    let w = avail / per_row as f32;
    for chunk in items.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (label, value, level) in chunk {
                ui.allocate_ui_with_layout(vec2(w, 44.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(w - 8.0);
                    stat(ui, label, value, *level);
                });
            }
        });
        ui.add_space(4.0);
    }
}

pub fn level_pill(ui: &mut Ui, level: Level) {
    let p = palette(ui);
    let color = level.color(p);
    let font = FontId::proportional(11.0);
    let galley = ui.painter().layout_no_wrap(level.label().to_string(), font, p.text);
    let size = vec2(galley.size().x + 22.0, 18.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 9.0, color.gamma_multiply(0.16));
    ui.painter().circle_filled(pos2(rect.left() + 9.0, rect.center().y), 3.5, color);
    ui.painter().galley(pos2(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
}

/// Horizontal bar meter, `frac` in 0..=1.
pub fn meter(ui: &mut Ui, frac: f32, color: Color32, height: f32) -> Response {
    let p = palette(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let r = (height / 2.0).min(4.0);
    ui.painter().rect_filled(rect, r, p.track);
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        let fill = Rect::from_min_size(rect.min, vec2((rect.width() * f).max(height.min(4.0)), height));
        ui.painter().rect_filled(fill, r, color);
    }
    resp
}

/// Stacked meter (e.g. memory used / cached / free). Segments are separated by a 2 px surface gap.
pub fn stacked_meter(ui: &mut Ui, parts: &[(f32, Color32)], height: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, p.track);
    let mut x = rect.left();
    for (frac, color) in parts {
        let w = rect.width() * frac.clamp(0.0, 1.0);
        if w < 1.0 {
            continue;
        }
        let seg = Rect::from_min_max(pos2(x, rect.top()), pos2((x + w - 2.0).max(x + 1.0), rect.bottom()));
        ui.painter().rect_filled(seg, 4.0, *color);
        x += w;
    }
}

pub fn legend_item(ui: &mut Ui, color: Color32, label: &str, value: &str) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().rect_filled(r, 2.0, color);
        ui.label(RichText::new(label).color(p.text_secondary));
        ui.label(RichText::new(value).strong().color(p.text));
    });
}

/// A circular gauge with the value in its centre.
pub fn ring_gauge(ui: &mut Ui, frac: Option<f32>, diameter: f32, color: Color32, center: &str, caption: &str) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let painter = ui.painter();
    let c = rect.center();
    let radius = diameter / 2.0 - 5.0;
    let stroke_w = 7.0;
    // 270° arc opening at the bottom.
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let n = 48;
        (0..=n)
            .map(|i| {
                let a = from + (to - from) * i as f32 / n as f32;
                c + vec2(a.cos(), a.sin()) * radius
            })
            .collect()
    };
    painter.add(Shape::line(arc(start, start + sweep), Stroke::new(stroke_w, p.track)));
    if let Some(f) = frac {
        let f = f.clamp(0.0, 1.0);
        if f > 0.005 {
            painter.add(Shape::line(arc(start, start + sweep * f), Stroke::new(stroke_w, color)));
        }
    }
    painter.text(c - vec2(0.0, 4.0), Align2::CENTER_CENTER, center, FontId::proportional(diameter * 0.2), p.text);
    painter.text(
        c + vec2(0.0, diameter * 0.17),
        Align2::CENTER_CENTER,
        caption,
        FontId::proportional(11.0),
        p.text_muted,
    );
}

pub fn info_banner(ui: &mut Ui, text: &str) {
    let p = palette(ui);
    Frame::new()
        .fill(p.accent_soft.gamma_multiply(if ui.visuals().dark_mode { 0.6 } else { 0.7 }))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icon_label(ui, Icon::Info, 16.0, p.accent);
                ui.add(egui::Label::new(RichText::new(text).color(p.text_secondary)).wrap());
            });
        });
}

pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, body: &str) {
    let p = palette(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        icon_label(ui, icon, 40.0, p.text_muted);
        ui.add_space(8.0);
        ui.label(RichText::new(title).strong().size(16.0).color(p.text));
        ui.add(egui::Label::new(RichText::new(body).color(p.text_secondary)).wrap());
        ui.add_space(40.0);
    });
}

/// Two-column key / value list.
pub fn kv_grid(ui: &mut Ui, id: &str, rows: &[(&str, String)]) {
    let p = palette(ui);
    egui::Grid::new(id).num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
        for (k, v) in rows {
            ui.label(RichText::new(*k).color(p.text_muted));
            ui.add(egui::Label::new(RichText::new(v).color(p.text)).wrap());
            ui.end_row();
        }
    });
}

/// Lay out `n` equal-width columns that collapse to fewer on narrow windows.
pub fn responsive_columns(ui: &mut Ui, n: usize, min_width: f32, mut add: impl FnMut(&mut Ui, usize)) {
    if n == 0 {
        return;
    }
    let gap = 12.0;
    let avail = ui.available_width();
    let cols = (((avail + gap) / (min_width + gap)).floor() as usize).clamp(1, n);
    let w = (avail - gap * (cols - 1) as f32) / cols as f32;
    let mut i = 0;
    while i < n {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for _ in 0..cols {
                if i >= n {
                    break;
                }
                ui.allocate_ui_with_layout(vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(w);
                    add(ui, i);
                });
                i += 1;
            }
        });
        ui.add_space(gap);
    }
}

// ---------------------------------------------------------------- icons

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Overview,
    Cpu,
    Memory,
    Gpu,
    Storage,
    Network,
    Sensors,
    Processes,
    Settings,
    Pause,
    Play,
    Info,
    Search,
    Logo,
}

pub fn icon_label(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_icon(ui.painter(), rect, icon, color);
    resp
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width();
    let c = rect.center();
    let w = (s / 12.0).max(1.2);
    let stroke = Stroke::new(w, color);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_min_max(rect.min + vec2(x0 * s, y0 * s), rect.min + vec2(x1 * s, y1 * s))
    };
    let pt = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    match icon {
        Icon::Overview => {
            for (x, y) in [(0.12, 0.12), (0.56, 0.12), (0.12, 0.56), (0.56, 0.56)] {
                painter.rect_stroke(r(x, y, x + 0.32, y + 0.32), s * 0.06, stroke, StrokeKind::Middle);
            }
        }
        Icon::Cpu => {
            painter.rect_stroke(r(0.22, 0.22, 0.78, 0.78), s * 0.08, stroke, StrokeKind::Middle);
            painter.rect_filled(r(0.38, 0.38, 0.62, 0.62), s * 0.03, color);
            for i in 0..3 {
                let f = 0.34 + i as f32 * 0.16;
                painter.line_segment([pt(f, 0.06), pt(f, 0.22)], stroke);
                painter.line_segment([pt(f, 0.78), pt(f, 0.94)], stroke);
                painter.line_segment([pt(0.06, f), pt(0.22, f)], stroke);
                painter.line_segment([pt(0.78, f), pt(0.94, f)], stroke);
            }
        }
        Icon::Memory => {
            painter.rect_stroke(r(0.06, 0.3, 0.94, 0.66), s * 0.05, stroke, StrokeKind::Middle);
            for i in 0..4 {
                let x = 0.18 + i as f32 * 0.18;
                painter.rect_filled(r(x, 0.39, x + 0.1, 0.57), 0.0, color);
            }
            for i in 0..6 {
                let x = 0.14 + i as f32 * 0.144;
                painter.line_segment([pt(x, 0.66), pt(x, 0.8)], stroke);
            }
        }
        Icon::Gpu => {
            painter.rect_stroke(r(0.06, 0.24, 0.94, 0.72), s * 0.06, stroke, StrokeKind::Middle);
            painter.circle_stroke(pt(0.62, 0.48), s * 0.14, stroke);
            painter.rect_filled(r(0.18, 0.38, 0.36, 0.58), s * 0.02, color);
            painter.line_segment([pt(0.2, 0.72), pt(0.2, 0.86)], stroke);
            painter.line_segment([pt(0.2, 0.86), pt(0.6, 0.86)], stroke);
        }
        Icon::Storage => {
            painter.rect_stroke(r(0.12, 0.2, 0.88, 0.8), s * 0.08, stroke, StrokeKind::Middle);
            painter.line_segment([pt(0.12, 0.56), pt(0.88, 0.56)], stroke);
            painter.circle_filled(pt(0.72, 0.68), s * 0.05, color);
            painter.circle_filled(pt(0.72, 0.38), s * 0.05, color);
        }
        Icon::Network => {
            painter.line_segment([pt(0.32, 0.88), pt(0.32, 0.14)], stroke);
            painter.line_segment([pt(0.32, 0.14), pt(0.16, 0.32)], stroke);
            painter.line_segment([pt(0.32, 0.14), pt(0.48, 0.32)], stroke);
            painter.line_segment([pt(0.68, 0.12), pt(0.68, 0.86)], stroke);
            painter.line_segment([pt(0.68, 0.86), pt(0.52, 0.68)], stroke);
            painter.line_segment([pt(0.68, 0.86), pt(0.84, 0.68)], stroke);
        }
        Icon::Sensors => {
            painter.rect_stroke(r(0.4, 0.06, 0.6, 0.66), s * 0.1, stroke, StrokeKind::Middle);
            painter.circle_filled(pt(0.5, 0.76), s * 0.16, color);
            painter.line_segment([pt(0.5, 0.3), pt(0.5, 0.7)], Stroke::new(w * 1.4, color));
            painter.line_segment([pt(0.68, 0.22), pt(0.8, 0.22)], stroke);
            painter.line_segment([pt(0.68, 0.38), pt(0.8, 0.38)], stroke);
        }
        Icon::Processes => {
            for i in 0..3 {
                let y = 0.22 + i as f32 * 0.28;
                painter.circle_filled(pt(0.14, y), s * 0.06, color);
                painter.line_segment([pt(0.3, y), pt(0.9, y)], stroke);
            }
        }
        Icon::Settings => {
            let teeth = 8;
            for i in 0..teeth {
                let a = i as f32 / teeth as f32 * std::f32::consts::TAU;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment([c + d * s * 0.28, c + d * s * 0.44], Stroke::new(w * 1.8, color));
            }
            painter.circle_stroke(c, s * 0.28, stroke);
            painter.circle_stroke(c, s * 0.11, stroke);
        }
        Icon::Pause => {
            painter.rect_filled(r(0.24, 0.18, 0.4, 0.82), s * 0.03, color);
            painter.rect_filled(r(0.6, 0.18, 0.76, 0.82), s * 0.03, color);
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![pt(0.26, 0.16), pt(0.84, 0.5), pt(0.26, 0.84)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Info => {
            painter.circle_stroke(c, s * 0.42, stroke);
            painter.circle_filled(pt(0.5, 0.3), s * 0.06, color);
            painter.line_segment([pt(0.5, 0.44), pt(0.5, 0.74)], Stroke::new(w * 1.3, color));
        }
        Icon::Search => {
            painter.circle_stroke(pt(0.42, 0.42), s * 0.26, stroke);
            painter.line_segment([pt(0.62, 0.62), pt(0.88, 0.88)], Stroke::new(w * 1.4, color));
        }
        Icon::Logo => paint_logo(painter, rect),
    }
}

/// The app mark: a rounded tile with a thermometer and a pulse line.
pub fn paint_logo(painter: &egui::Painter, rect: Rect) {
    let s = rect.width();
    let pt = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    painter.rect_filled(rect, s * 0.24, Color32::from_rgb(0x2a, 0x78, 0xd6));
    let white = Color32::WHITE;
    painter.rect_filled(Rect::from_min_max(pt(0.27, 0.16), pt(0.43, 0.62)), s * 0.08, white);
    painter.circle_filled(pt(0.35, 0.7), s * 0.14, white);
    painter.circle_filled(pt(0.35, 0.7), s * 0.08, Color32::from_rgb(0xeb, 0x68, 0x34));
    painter.rect_filled(Rect::from_min_max(pt(0.32, 0.36), pt(0.38, 0.66)), 0.0, Color32::from_rgb(0xeb, 0x68, 0x34));
    let pulse = vec![pt(0.5, 0.56), pt(0.6, 0.56), pt(0.66, 0.34), pt(0.73, 0.72), pt(0.79, 0.5), pt(0.88, 0.5)];
    painter.add(Shape::line(pulse, Stroke::new((s * 0.06).max(1.5), white)));
}

// ---------------------------------------------------------------- table

/// Column spec: a weight for flexible width plus a minimum in points.
pub struct Col {
    pub title: &'static str,
    pub weight: f32,
    pub min: f32,
    pub right: bool,
}

pub const fn col(title: &'static str, weight: f32, min: f32, right: bool) -> Col {
    Col { title, weight, min, right }
}

/// Resolve column widths so the table always spans the available width.
pub fn column_widths(cols: &[Col], avail: f32, gap: f32) -> Vec<f32> {
    let gaps = gap * (cols.len().saturating_sub(1)) as f32;
    let min_total: f32 = cols.iter().map(|c| c.min).sum();
    let extra = (avail - gaps - min_total).max(0.0);
    let weights: f32 = cols.iter().map(|c| c.weight).sum::<f32>().max(1e-6);
    cols.iter().map(|c| c.min + extra * c.weight / weights).collect()
}

/// A full-width table row. `cell` draws column `i` into a UI of the right width.
/// Returns the row's response so callers can add hover or click behaviour.
pub fn table_row(
    ui: &mut Ui,
    widths: &[f32],
    height: f32,
    index: usize,
    header: bool,
    mut cell: impl FnMut(&mut Ui, usize),
) -> Response {
    let p = palette(ui);
    let gap = 12.0;
    let total: f32 = widths.iter().sum::<f32>() + gap * (widths.len().saturating_sub(1)) as f32;
    let (rect, resp) = ui.allocate_exact_size(vec2(total, height), Sense::hover());
    if !header && index % 2 == 1 {
        ui.painter().rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
    }
    if !header && resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, p.track.gamma_multiply(0.6));
    }
    if header {
        ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, p.grid));
    }
    let mut x = rect.left() + 6.0;
    for (i, w) in widths.iter().enumerate() {
        let cell_rect = Rect::from_min_size(pos2(x, rect.top()), vec2(*w - if i == 0 { 6.0 } else { 0.0 }, height));
        let mut cui = ui.new_child(
            egui::UiBuilder::new().max_rect(cell_rect).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        cui.set_clip_rect(cell_rect.intersect(ui.clip_rect()));
        cell(&mut cui, i);
        x += w + gap;
    }
    resp
}

/// Natural ordering so "Core 2" sorts before "Core 10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let o = na.parse::<u128>().unwrap_or(0).cmp(&nb.parse::<u128>().unwrap_or(0));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["Core 10", "Core 2", "Core 1", "core 11"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["Core 1", "Core 2", "Core 10", "core 11"]);
    }

    #[test]
    fn columns_fill_width() {
        let cols = [col("a", 2.0, 100.0, false), col("b", 1.0, 50.0, true)];
        let w = column_widths(&cols, 400.0, 10.0);
        assert!((w.iter().sum::<f32>() + 10.0 - 400.0).abs() < 0.01);
        assert!(w[0] > w[1]);
    }
}
