//! Application shell: navigation, top bar, status bar, shortcuts, persistence.

mod chart;
#[cfg(feature = "dev-screenshot")]
mod devshot;
mod pages;
mod theme;
mod widgets;

use std::path::PathBuf;
use std::time::Duration;

use egui::{Align, CornerRadius, Frame, Key, Layout, Margin, RichText, Sense, Stroke, ThemePreference, vec2};
use serde::{Deserialize, Serialize};

use crate::sampler::Sampler;
use crate::settings::{Settings, ThemeChoice};
use theme::palette;
use widgets::{Icon, paint_icon};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Page {
    #[default]
    Overview,
    Cpu,
    Memory,
    Gpu,
    Storage,
    Network,
    Sensors,
    Processes,
    Settings,
}

impl Page {
    const NAV: [Page; 9] = [
        Page::Overview,
        Page::Cpu,
        Page::Memory,
        Page::Gpu,
        Page::Storage,
        Page::Network,
        Page::Sensors,
        Page::Processes,
        Page::Settings,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Cpu => "Processor",
            Page::Memory => "Memory",
            Page::Gpu => "Graphics",
            Page::Storage => "Storage",
            Page::Network => "Network",
            Page::Sensors => "Sensors",
            Page::Processes => "Processes",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::Overview => Icon::Overview,
            Page::Cpu => Icon::Cpu,
            Page::Memory => Icon::Memory,
            Page::Gpu => Icon::Gpu,
            Page::Storage => Icon::Storage,
            Page::Network => Icon::Network,
            Page::Sensors => Icon::Sensors,
            Page::Processes => Icon::Processes,
            Page::Settings => Icon::Settings,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcSort {
    Name,
    Pid,
    Cpu,
    Memory,
    Threads,
    Disk,
}

pub struct App {
    sampler: Sampler,
    pub settings: Settings,
    applied: Settings,
    pub page: Page,
    pub proc_filter: String,
    pub proc_sort: (ProcSort, bool),
    pub sensor_filter: String,
    pub toast: Option<(String, f64)>,
    reset_stats: bool,
    #[cfg(feature = "dev-screenshot")]
    devshot: devshot::DevShot,
}

const KEY_SETTINGS: &str = "settings";
const KEY_PAGE: &str = "page";

impl App {
    fn new(cc: &eframe::CreationContext<'_>, start_page: Option<Page>) -> Self {
        theme::install(&cc.egui_ctx);
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, KEY_SETTINGS))
            .unwrap_or_default();
        let settings = settings.sanitized();
        let page = start_page.or_else(|| cc.storage.and_then(|s| eframe::get_value(s, KEY_PAGE))).unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let sampler = Sampler::start(Duration::from_millis(settings.interval_ms), settings.history_secs as f64, move || {
            ctx.request_repaint()
        });
        let mut app = Self {
            sampler,
            applied: settings.clone(),
            settings,
            page,
            proc_filter: String::new(),
            proc_sort: (ProcSort::Cpu, true),
            sensor_filter: String::new(),
            toast: None,
            reset_stats: false,
            #[cfg(feature = "dev-screenshot")]
            devshot: devshot::DevShot::from_env(),
        };
        app.apply_settings(&cc.egui_ctx, true);
        app
    }

    /// Push changed settings to egui and the sampler.
    fn apply_settings(&mut self, ctx: &egui::Context, force: bool) {
        let s = &self.settings;
        let a = &self.applied;
        if force || s.theme != a.theme {
            ctx.set_theme(match s.theme {
                ThemeChoice::System => ThemePreference::System,
                ThemeChoice::Dark => ThemePreference::Dark,
                ThemeChoice::Light => ThemePreference::Light,
            });
        }
        if force || s.ui_scale != a.ui_scale {
            ctx.set_zoom_factor(s.ui_scale);
        }
        if force || s.interval_ms != a.interval_ms {
            self.sampler.set_interval(Duration::from_millis(s.interval_ms));
        }
        if force || s.history_secs != a.history_secs {
            self.sampler.set_window(s.history_secs as f64);
        }
        if force || s.log_enabled != a.log_enabled || s.log_path != a.log_path {
            let path = (s.log_enabled && !s.log_path.trim().is_empty()).then(|| PathBuf::from(s.log_path.trim()));
            self.sampler.set_log_path(path);
        }
        self.applied = self.settings.clone();
    }

    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        self.toast = Some((text.into(), ctx.input(|i| i.time) + 2.5));
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        ctx.input(|i| {
            if i.modifiers.command {
                for (k, page) in keys.iter().zip(Page::NAV) {
                    if i.key_pressed(*k) {
                        self.page = page;
                    }
                }
            }
        });
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::P)) {
            self.sampler.set_paused(!self.sampler.paused());
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::Comma)) {
            self.page = Page::Settings;
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui);
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(212.0)
            .frame(Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(12, 16)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                    widgets::paint_logo(ui.painter(), r);
                    ui.add_space(2.0);
                    ui.label(RichText::new(crate::APP_NAME).strong().size(17.0).color(p.text));
                });
                ui.add_space(18.0);
                for (i, page) in Page::NAV.into_iter().enumerate() {
                    if page == Page::Settings {
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                    }
                    let selected = self.page == page;
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                    let resp = resp.on_hover_text(format!("{}  (Ctrl+{})", page.title(), i + 1));
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, page.title()));
                    let fill = if selected {
                        p.accent_soft
                    } else if resp.hovered() {
                        p.track
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, 7.0, fill);
                    if selected {
                        let bar = egui::Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(3.0, rect.height() - 16.0));
                        ui.painter().rect_filled(bar, 2.0, p.accent);
                    }
                    if resp.has_focus() {
                        ui.painter().rect_stroke(rect, 7.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
                    }
                    let color = if selected { p.accent } else { p.text_secondary };
                    let icon_rect = egui::Rect::from_center_size(rect.left_center() + vec2(22.0, 0.0), vec2(17.0, 17.0));
                    paint_icon(ui.painter(), icon_rect, page.icon(), color);
                    ui.painter().text(
                        rect.left_center() + vec2(42.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        page.title(),
                        egui::FontId::proportional(13.5),
                        if selected { p.text } else { p.text_secondary },
                    );
                    if resp.clicked() {
                        self.page = page;
                    }
                }
            });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, subtitle: &str) {
        let p = palette(ui);
        ui.horizontal(|ui| {
            // Leave room for the action buttons; the subtitle truncates with a tooltip.
            let title_w = (ui.available_width() - 240.0).max(120.0);
            ui.allocate_ui_with_layout(vec2(title_w, 48.0), Layout::top_down(Align::Min), |ui| {
                ui.set_max_width(title_w);
                ui.label(RichText::new(self.page.title()).size(22.0).strong().color(p.text));
                ui.add(egui::Label::new(RichText::new(subtitle).color(p.text_muted)).truncate()).on_hover_text(subtitle);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let paused = self.sampler.paused();
                let (icon, label) = if paused { (Icon::Play, "Resume") } else { (Icon::Pause, "Pause") };
                let btn = ui
                    .add(egui::Button::new(format!("      {label}")).min_size(vec2(96.0, 30.0)))
                    .on_hover_text(if paused { "Resume live updates (Ctrl+P)" } else { "Freeze the readings (Ctrl+P)" });
                let ir = egui::Rect::from_center_size(btn.rect.left_center() + vec2(18.0, 0.0), vec2(12.0, 12.0));
                paint_icon(ui.painter(), ir, icon, p.text);
                if btn.clicked() {
                    self.sampler.set_paused(!paused);
                }
                if ui
                    .add(egui::Button::new("Copy report").min_size(vec2(0.0, 30.0)))
                    .on_hover_text("Copy every current reading as JSON, for bug reports or sharing")
                    .clicked()
                {
                    let g = self.sampler.shared.read().unwrap();
                    let json = serde_json::to_string_pretty(&serde_json::json!({
                        "app": format!("{} {}", crate::APP_NAME, env!("CARGO_PKG_VERSION")),
                        "info": g.info,
                        "snapshot": g.latest.as_deref(),
                    }))
                    .unwrap_or_default();
                    drop(g);
                    ui.ctx().copy_text(json);
                    let ctx = ui.ctx().clone();
                    self.toast(&ctx, "Report copied to the clipboard");
                }
            });
        });
        ui.add_space(12.0);
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let p = palette(ui);
        let g = self.sampler.shared.read().unwrap();
        let (samples, cost, log_error) = (g.samples, g.last_cost, g.log_error.clone());
        drop(g);
        egui::Panel::bottom("status")
            .exact_size(28.0)
            .frame(Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(14, 4)).stroke(Stroke::new(1.0, p.border)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let paused = self.sampler.paused();
                    let (dot, text) = if paused {
                        (p.warning, "Paused".to_string())
                    } else {
                        (p.good, format!("Live · every {}", interval_label(self.settings.interval_ms)))
                    };
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, dot);
                    ui.label(RichText::new(text).small().color(p.text_secondary));
                    ui.label(RichText::new("·").small().color(p.text_muted));
                    ui.label(RichText::new(format!("sample took {:.1} ms", cost.as_secs_f64() * 1000.0)).small().color(p.text_muted))
                        .on_hover_text("Time spent reading all sensors in the last pass");
                    ui.label(RichText::new("·").small().color(p.text_muted));
                    ui.label(RichText::new(format!("{} samples", crate::format::count(samples))).small().color(p.text_muted));
                    if self.settings.log_enabled {
                        ui.label(RichText::new("·").small().color(p.text_muted));
                        match &log_error {
                            Some(e) => ui.label(RichText::new(format!("CSV log failed: {e}")).small().color(p.critical)),
                            None => ui.label(RichText::new("Logging to CSV").small().color(p.text_secondary)),
                        };
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().color(p.text_muted));
                    });
                });
            });
    }

    fn paint_toast(&mut self, ui: &mut egui::Ui) {
        let now = ui.input(|i| i.time);
        let Some((text, until)) = self.toast.clone() else { return };
        if now > until {
            self.toast = None;
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        let p = palette(ui);
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -44.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(p.text)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(text).color(p.surface));
                    });
            });
    }
}

pub fn interval_label(ms: u64) -> String {
    if ms < 1000 { format!("{ms} ms") } else { format!("{} s", ms / 1000) }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(feature = "dev-screenshot")]
        self.devshot.tick(&ctx);
        self.shortcuts(&ctx);
        self.sidebar(ui);
        self.status_bar(ui);

        let p = palette(ui);
        let shared = self.sampler.shared.clone();
        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.page).inner_margin(Margin { left: 24, right: 24, top: 18, bottom: 0 }))
            .show(ui, |ui| {
                let guard = shared.read().unwrap();
                let info = guard.info.clone();
                let latest = guard.latest.clone();
                let subtitle = if info.hostname.is_empty() {
                    "Starting…".to_string()
                } else {
                    format!("{} · {} {}", info.hostname, info.os_name, info.os_version)
                };
                self.top_bar(ui, &subtitle);
                let content_w = ui.available_width().min(1400.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // Pin the width: a vertical scroll area does not bound its content horizontally.
                    ui.set_width(content_w - 10.0);
                    if self.page == Page::Settings {
                        drop(guard);
                        pages::settings(ui, self);
                    } else if let Some(snap) = latest {
                        let view = pages::View { s: &snap, info: &info, h: &guard.history, settings: &self.settings, now: snap.t };
                        if let Some(next) = pages::show(ui, self.page, &view, &mut pages::PageState {
                            proc_filter: &mut self.proc_filter,
                            proc_sort: &mut self.proc_sort,
                            sensor_filter: &mut self.sensor_filter,
                            reset_stats: &mut self.reset_stats,
                        }) {
                            self.page = next;
                        }
                    } else {
                        widgets::empty_state(ui, Icon::Logo, "Reading sensors…", "The first sample is being collected.");
                    }
                    ui.add_space(24.0);
                });
            });
        if std::mem::take(&mut self.reset_stats) {
            shared.write().unwrap().history.reset_stats();
            self.toast(&ctx, "Min, max and average were reset");
        }
        self.paint_toast(ui);
        if self.settings != self.applied {
            self.apply_settings(&ctx, false);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, KEY_SETTINGS, &self.settings);
        eframe::set_value(storage, KEY_PAGE, &self.page);
    }
}

/// The window icon, rasterised from the same geometry as the in-app logo.
fn window_icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let blue = [0x2a, 0x78, 0xd6];
    let orange = [0xeb, 0x68, 0x34];
    let white = [0xff, 0xff, 0xff];
    let inside_round_rect = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let cx = x.clamp(x0 + r, x1 - r);
        let cy = y.clamp(y0 + r, y1 - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r && x >= x0 && x <= x1 && y >= y0 && y <= y1
    };
    let seg_dist = |px: f32, py: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
    };
    let pulse = [(0.5, 0.56), (0.6, 0.56), (0.66, 0.34), (0.73, 0.72), (0.79, 0.5), (0.88, 0.5)];
    for yi in 0..n {
        for xi in 0..n {
            let (x, y) = ((xi as f32 + 0.5) / n as f32, (yi as f32 + 0.5) / n as f32);
            let mut c = None;
            if inside_round_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.24) {
                c = Some(blue);
            }
            if inside_round_rect(x, y, 0.27, 0.16, 0.43, 0.62, 0.08) || (x - 0.35).powi(2) + (y - 0.7).powi(2) <= 0.14f32.powi(2) {
                c = Some(white);
            }
            if (x - 0.35).powi(2) + (y - 0.7).powi(2) <= 0.08f32.powi(2) || (x >= 0.32 && x <= 0.38 && y >= 0.36 && y <= 0.66) {
                c = Some(orange);
            }
            if pulse.windows(2).any(|w| seg_dist(x, y, w[0], w[1]) <= 0.035) {
                c = Some(white);
            }
            if let Some(c) = c {
                let i = (yi * n + xi) * 4;
                rgba[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    }
    egui::IconData { rgba, width: n as u32, height: n as u32 }
}

pub fn run(start_page: Option<Page>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(crate::APP_NAME)
            .with_app_id("opentemp")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(crate::APP_NAME, options, Box::new(|cc| Ok(Box::new(App::new(cc, start_page)))))
}

/// Parse a page name given on the command line (`--page cpu`).
pub fn parse_page(name: &str) -> Option<Page> {
    let n = name.to_ascii_lowercase();
    Page::NAV.into_iter().find(|p| {
        let t = p.title().to_ascii_lowercase();
        t == n || format!("{p:?}").to_ascii_lowercase() == n
    })
}
