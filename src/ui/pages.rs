//! Page bodies. Each page is a pure function of the latest snapshot, the
//! history and the settings; navigation requests are returned to the shell.

use egui::{RichText, Sense, Ui, vec2};

use super::chart::{Chart, Line, Scale, sparkline};
use super::theme::{Level, palette};
use super::widgets::*;
use super::{App, Page, ProcSort};
use crate::format as f;
use crate::history::History;
use crate::model::*;
use crate::settings::{Settings, TempUnit, ThemeChoice};

pub struct View<'a> {
    pub s: &'a Snapshot,
    pub info: &'a StaticInfo,
    pub h: &'a History,
    pub settings: &'a Settings,
    pub now: f64,
}

pub struct PageState<'a> {
    pub proc_filter: &'a mut String,
    pub proc_sort: &'a mut (ProcSort, bool),
    pub sensor_filter: &'a mut String,
    pub reset_stats: &'a mut bool,
}

impl View<'_> {
    fn temp(&self, c: f32) -> String {
        f::temp(c, self.settings.temp_unit)
    }
    fn temp_level(&self, c: f32) -> Level {
        Level::for_temp(c, self.settings.warn_temp, self.settings.hot_temp)
    }
    fn window(&self) -> f64 {
        self.h.window_secs
    }
    fn chart<'a>(&'a self, lines: Vec<Line<'a>>, scale: Scale, height: f32, fmt: &'a dyn Fn(f64) -> String) -> Chart<'a> {
        Chart { lines, scale, height, window_secs: self.window(), now: self.now, fmt, fill: true }
    }
    fn notes(&self, ui: &mut Ui, area: &str) {
        for n in self.s.notes.iter().filter(|n| n.area == area) {
            info_banner(ui, &n.text);
            ui.add_space(10.0);
        }
    }
}

pub fn show(ui: &mut Ui, page: Page, v: &View, st: &mut PageState) -> Option<Page> {
    match page {
        Page::Overview => return overview(ui, v),
        Page::Cpu => cpu(ui, v),
        Page::Memory => memory(ui, v),
        Page::Gpu => gpu(ui, v),
        Page::Storage => storage(ui, v),
        Page::Network => network(ui, v),
        Page::Sensors => sensors(ui, v, st.sensor_filter, st.reset_stats),
        Page::Processes => processes(ui, v, st.proc_filter, st.proc_sort),
        Page::Settings => {}
    }
    None
}

fn pct_fmt(v: f64) -> String {
    format!("{v:.0}%")
}

// ------------------------------------------------------------------ Overview

fn overview(ui: &mut Ui, v: &View) -> Option<Page> {
    let p = palette(ui);
    let s = v.s;
    let mut go = None;
    let mut cards: Vec<Box<dyn Fn(&mut Ui) -> Option<Page> + '_>> = Vec::new();

    cards.push(Box::new(|ui| {
        let r = link_card(ui, "ov-cpu", "Open processor details", |ui| {
            card_header(ui, Icon::Cpu, "Processor", Some(&v.info.cpu.model));
            ui.horizontal(|ui| {
                let u = s.cpu.usage.unwrap_or(0.0);
                ring_gauge(ui, s.cpu.usage.map(|u| u / 100.0), 108.0, Level::for_load(u).bar(p), &f::percent(u), "utilization");
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    stat(ui, "Effective clock", &s.cpu.avg_mhz.map(f::mhz).unwrap_or_else(|| "—".into()), None);
                    match s.cpu.temperature {
                        Some(t) => stat(ui, "Temperature", &v.temp(t), Some(v.temp_level(t))),
                        None => stat(ui, "Package power", &s.cpu.package_power_w.map(f::watts).unwrap_or_else(|| "—".into()), None),
                    }
                    stat(ui, "Cores / threads", &format!("{} / {}", v.info.cpu.physical_cores, v.info.cpu.logical_cores), None);
                });
            });
            sparkline(ui, v.h.get("cpu.usage"), vec2(ui.available_width(), 36.0), p.series[0], Some((0.0, 100.0)), v.window().min(60.0), v.now);
        });
        r.clicked().then_some(Page::Cpu)
    }));

    cards.push(Box::new(|ui| {
        let m = &s.memory;
        let r = link_card(ui, "ov-mem", "Open memory details", |ui| {
            card_header(ui, Icon::Memory, "Memory", Some(&format!("{} installed", f::bytes_u(m.total))));
            ui.horizontal(|ui| {
                let u = m.used_pct();
                ring_gauge(ui, Some(u / 100.0), 108.0, Level::for_load(u).bar(p), &f::percent(u), "in use");
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    stat(ui, "In use", &f::bytes_u(m.used), None);
                    stat(ui, "Available", &f::bytes_u(m.available), None);
                    if let (Some(u), Some(t)) = (m.swap_used, m.swap_total) {
                        stat(ui, if cfg!(windows) { "Page file" } else { "Swap" }, &format!("{} / {}", f::bytes_u(u), f::bytes_u(t)), None);
                    }
                });
            });
            sparkline(ui, v.h.get("mem.used_pct"), vec2(ui.available_width(), 36.0), p.series[0], Some((0.0, 100.0)), v.window().min(60.0), v.now);
        });
        r.clicked().then_some(Page::Memory)
    }));

    for (gi, g) in s.gpus.iter().enumerate() {
        cards.push(Box::new(move |ui| {
            let r = link_card(ui, ("ov-gpu", gi), "Open graphics details", |ui| {
                card_header(ui, Icon::Gpu, "Graphics", Some(&g.name));
                ui.horizontal(|ui| {
                    let u = g.usage.unwrap_or(0.0);
                    ring_gauge(ui, g.usage.map(|u| u / 100.0), 108.0, Level::for_load(u).bar(p), &g.usage.map(f::percent).unwrap_or("—".into()), "utilization");
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        match g.temperature {
                            Some(t) => stat(ui, "Temperature", &v.temp(t), Some(v.temp_level(t))),
                            None => stat(ui, "Temperature", "—", None),
                        }
                        stat(ui, "Core clock", &g.core_mhz.map(f::mhz).unwrap_or_else(|| "—".into()), None);
                        let mem = match (g.dedicated_used, g.dedicated_total) {
                            (Some(u), Some(t)) => format!("{} / {}", f::bytes_u(u), f::bytes_u(t)),
                            (Some(u), None) => f::bytes_u(u),
                            _ => "—".into(),
                        };
                        stat(ui, if g.integrated { "Reserved memory" } else { "Video memory" }, &mem, None);
                    });
                });
                sparkline(ui, v.h.get(&format!("gpu.{}.usage", g.key)), vec2(ui.available_width(), 36.0), p.series[0], Some((0.0, 100.0)), v.window().min(60.0), v.now);
            });
            r.clicked().then_some(Page::Gpu)
        }));
    }

    cards.push(Box::new(|ui| {
        let r = link_card(ui, "ov-net", "Open network details", |ui| {
            let up: Vec<&NetSample> = s.networks.iter().filter(|n| n.up).collect();
            let trailing = up.first().map(|n| n.name.clone()).unwrap_or_else(|| "Disconnected".into());
            card_header(ui, Icon::Network, "Network", Some(&trailing));
            let rx: f64 = s.networks.iter().map(|n| n.rx_bps).sum();
            let tx: f64 = s.networks.iter().map(|n| n.tx_bps).sum();
            ui.horizontal(|ui| {
                legend_item(ui, p.series[0], "Download", &f::bit_rate(rx));
                ui.add_space(12.0);
                legend_item(ui, p.series[1], "Upload", &f::bit_rate(tx));
            });
            ui.add_space(4.0);
            let fmt = |x: f64| f::bit_rate(x);
            let mut c = v.chart(
                vec![
                    Line { name: "Download", series: v.h.get("net.rx"), color: p.series[0] },
                    Line { name: "Upload", series: v.h.get("net.tx"), color: p.series[1] },
                ],
                Scale::AutoFromZero { min_span: 125_000.0 },
                92.0,
                &fmt,
            );
            c.fill = false;
            c.show(ui);
        });
        r.clicked().then_some(Page::Network)
    }));

    cards.push(Box::new(|ui| {
        let r = link_card(ui, "ov-disk", "Open storage details", |ui| {
            card_header(ui, Icon::Storage, "Storage", Some(&format!("{} volume{}", s.disks.len(), if s.disks.len() == 1 { "" } else { "s" })));
            if s.disks.is_empty() {
                ui.label(RichText::new("No volumes found").color(p.text_muted));
            }
            for d in s.disks.iter().take(4) {
                let used = d.total.saturating_sub(d.free);
                let frac = used as f32 / d.total.max(1) as f32;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&d.name).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} free", f::bytes_u(d.free))).small().color(p.text_muted));
                    });
                });
                meter(ui, frac, capacity_level(frac).bar(p), 8.0);
                if let (Some(r), Some(w)) = (d.read_bps, d.write_bps) {
                    ui.label(RichText::new(format!("Read {}  ·  Write {}", f::byte_rate(r), f::byte_rate(w))).small().color(p.text_secondary));
                }
                ui.add_space(4.0);
            }
            if let Some(t) = s.drives.iter().find_map(|d| d.temperature) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Drive temperature").small().color(p.text_muted));
                    ui.label(RichText::new(v.temp(t)).small().strong().color(p.text));
                });
            }
        });
        r.clicked().then_some(Page::Storage)
    }));

    cards.push(Box::new(|ui| {
        card(ui, |ui| {
            card_header(ui, Icon::Info, "System", None);
            let mut rows = vec![
                ("Computer", v.info.hostname.clone()),
                ("Operating system", format!("{} {}", v.info.os_name, v.info.os_version)),
            ];
            if let Some(u) = s.system.uptime_secs {
                rows.push(("Uptime", f::duration(u)));
            }
            if let Some(n) = s.system.processes {
                rows.push(("Processes", f::count(n as u64)));
            }
            if let Some(n) = s.system.threads {
                rows.push(("Threads", f::count(n as u64)));
            }
            if let Some(b) = &s.battery {
                let pct = b.percent.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "—".into());
                let state = if b.charging { "charging" } else if b.on_ac { "plugged in" } else { "on battery" };
                rows.push(("Battery", format!("{pct}, {state}")));
            }
            kv_grid(ui, "ov-sys", &rows);
        });
        None
    }));

    responsive_columns(ui, cards.len(), 300.0, |ui, i| {
        if let Some(pg) = cards[i](ui) {
            go = Some(pg);
        }
    });
    go
}

fn capacity_level(frac: f32) -> Level {
    match frac {
        x if x >= 0.97 => Level::Critical,
        x if x >= 0.90 => Level::High,
        _ => Level::Normal,
    }
}

// ----------------------------------------------------------------------- CPU

fn cpu(ui: &mut Ui, v: &View) {
    let p = palette(ui);
    let s = v.s;
    let info = &v.info.cpu;
    v.notes(ui, "cpu");

    let mut stats: Vec<(&str, String, Option<Level>)> = vec![
        ("Utilization", s.cpu.usage.map(f::percent).unwrap_or("—".into()), s.cpu.usage.map(Level::for_load)),
        ("Effective clock", s.cpu.avg_mhz.map(f::mhz).unwrap_or("—".into()), None),
        ("Fastest core", s.cpu.max_core_mhz.map(f::mhz).unwrap_or("—".into()), None),
    ];
    if let Some(t) = s.cpu.temperature {
        stats.push(("Temperature", v.temp(t), Some(v.temp_level(t))));
    }
    if let Some(w) = s.cpu.package_power_w {
        stats.push(("Package power", f::watts(w), None));
    }
    if let Some(l) = s.cpu.load_avg {
        stats.push(("Load average", format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2]), None));
    }
    if let Some(n) = s.system.processes {
        stats.push(("Processes", f::count(n as u64), None));
    }
    if let Some(n) = s.system.threads {
        stats.push(("Threads", f::count(n as u64), None));
    }
    if let Some(n) = s.system.handles {
        stats.push(("Handles", f::count(n as u64), None));
    }
    if let Some(n) = s.system.context_switches_per_sec {
        stats.push(("Context switches", format!("{}/s", f::count(n as u64)), None));
    }
    if let Some(u) = s.system.uptime_secs {
        stats.push(("Uptime", f::duration(u), None));
    }
    card(ui, |ui| stat_row(ui, &stats));
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Cpu, "Utilization", Some(&info.model));
        v.chart(vec![Line { name: "Utilization", series: v.h.get("cpu.usage"), color: p.series[0] }], Scale::Fixed(0.0, 100.0), 180.0, &pct_fmt).show(ui);
    });
    ui.add_space(12.0);

    let mhz_fmt = |x: f64| f::mhz(x);
    let w_fmt = |x: f64| format!("{x:.0} W");
    let unit = v.settings.temp_unit;
    let t_fmt = move |x: f64| f::temp(x as f32, unit);
    let mut panels: Vec<(&str, &str, &dyn Fn(f64) -> String, Scale)> = vec![("Effective clock", "cpu.mhz", &mhz_fmt, Scale::AutoPadded { min_span: 500.0 })];
    if v.h.get("cpu.power").is_some() {
        panels.push(("Package power", "cpu.power", &w_fmt, Scale::AutoFromZero { min_span: 10.0 }));
    }
    if v.h.get("cpu.temp").is_some() {
        panels.push(("Temperature", "cpu.temp", &t_fmt, Scale::AutoPadded { min_span: 10.0 }));
    }
    let n = panels.len();
    let mut panels: Vec<Option<_>> = panels.into_iter().map(Some).collect();
    responsive_columns(ui, n, 320.0, |ui, i| {
        let (title, key, fmt, scale) = panels[i].take().unwrap();
        card(ui, |ui| {
            card_header(ui, Icon::Sensors, title, None);
            v.chart(vec![Line { name: title, series: v.h.get(key), color: p.series[0] }], scale, 130.0, fmt).show(ui);
        });
    });

    // Per-core tiles.
    card(ui, |ui| {
        let classes = &info.core_classes;
        let top_class = classes.iter().flatten().max().copied();
        let hybrid = classes.iter().flatten().collect::<std::collections::BTreeSet<_>>().len() > 1;
        card_header(ui, Icon::Cpu, "Logical processors", Some(&format!("{} threads", s.cpu.per_core.len())));
        if hybrid {
            ui.label(RichText::new("This processor mixes core types. P marks the higher-performance class, E the more efficient one.").small().color(p.text_muted));
            ui.add_space(6.0);
        }
        responsive_columns(ui, s.cpu.per_core.len(), 118.0, |ui, i| {
            let c = &s.cpu.per_core[i];
            let tag = classes.get(i).copied().flatten().filter(|_| hybrid).map(|cl| if Some(cl) == top_class { "P" } else { "E" });
            egui::Frame::new().fill(p.surface_raised).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("CPU {i}")).small().color(p.text_muted));
                    if let Some(t) = tag {
                        ui.label(RichText::new(t).small().strong().color(p.accent)).on_hover_text(if t == "P" { "Performance core" } else { "Efficiency core" });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(f::percent(c.usage)).strong().color(p.text));
                    });
                });
                meter(ui, c.usage / 100.0, Level::for_load(c.usage).bar(p), 6.0);
                if v.settings.show_per_core_clock {
                    if let Some(m) = c.mhz {
                        ui.label(RichText::new(f::mhz(m)).small().color(p.text_secondary));
                    }
                }
                sparkline(ui, v.h.get(&format!("cpu.core{i}")), vec2(ui.available_width(), 22.0), p.series[0], Some((0.0, 100.0)), v.window().min(120.0), v.now);
            });
        });
    });

    card(ui, |ui| {
        card_header(ui, Icon::Info, "Specifications", None);
        let mut rows = vec![
            ("Model", info.model.clone()),
            ("Vendor", info.vendor.clone()),
            ("Cores", format!("{} physical, {} logical", info.physical_cores, info.logical_cores)),
        ];
        if info.sockets > 1 {
            rows.push(("Sockets", info.sockets.to_string()));
        }
        if let Some(b) = info.base_mhz {
            rows.push(("Base clock", f::mhz(b)));
        }
        if let Some(m) = info.max_mhz {
            rows.push(("Maximum clock", f::mhz(m)));
        }
        for (label, val) in [("L1 cache", info.l1_bytes), ("L2 cache", info.l2_bytes), ("L3 cache", info.l3_bytes)] {
            if let Some(b) = val {
                rows.push((label, f::bytes_u(b)));
            }
        }
        rows.push(("Architecture", v.info.arch.clone()));
        if !info.features.is_empty() {
            rows.push(("Instruction sets", info.features.join(", ")));
        }
        kv_grid(ui, "cpu-spec", &rows);
    });
}

// -------------------------------------------------------------------- Memory

fn memory(ui: &mut Ui, v: &View) {
    let p = palette(ui);
    let m = &v.s.memory;
    let mut stats = vec![
        ("In use", f::bytes_u(m.used), Some(Level::for_load(m.used_pct()))),
        ("Available", f::bytes_u(m.available), None),
        ("Total", f::bytes_u(m.total), None),
    ];
    if let Some(c) = m.cached {
        stats.push(("Cached", f::bytes_u(c), None));
    }
    if let (Some(c), Some(l)) = (m.committed, m.commit_limit) {
        stats.push(("Committed", format!("{} / {}", f::bytes_u(c), f::bytes_u(l)), None));
    }
    if let (Some(u), Some(t)) = (m.swap_used, m.swap_total) {
        stats.push((if cfg!(windows) { "Page file" } else { "Swap" }, format!("{} / {}", f::bytes_u(u), f::bytes_u(t)), None));
    }
    card(ui, |ui| stat_row(ui, &stats));
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Memory, "Composition", None);
        let total = m.total.max(1) as f32;
        let cached = m.cached.unwrap_or(0).min(m.available);
        let free = m.available.saturating_sub(cached);
        stacked_meter(ui, &[(m.used as f32 / total, p.series[0]), (cached as f32 / total, p.series[2])], 18.0);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            legend_item(ui, p.series[0], "In use", &f::bytes_u(m.used));
            ui.add_space(16.0);
            legend_item(ui, p.series[2], "Cached (reclaimable)", &f::bytes_u(cached));
            ui.add_space(16.0);
            legend_item(ui, p.track, "Free", &f::bytes_u(free));
        });
    });
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Memory, "Memory in use", None);
        v.chart(vec![Line { name: "In use", series: v.h.get("mem.used_pct"), color: p.series[0] }], Scale::Fixed(0.0, 100.0), 180.0, &pct_fmt).show(ui);
    });
    if v.h.get("mem.swap_pct").is_some() {
        ui.add_space(12.0);
        card(ui, |ui| {
            card_header(ui, Icon::Storage, if cfg!(windows) { "Page file usage" } else { "Swap usage" }, None);
            v.chart(vec![Line { name: "Used", series: v.h.get("mem.swap_pct"), color: p.series[0] }], Scale::Fixed(0.0, 100.0), 120.0, &pct_fmt).show(ui);
        });
    }
}

// ----------------------------------------------------------------------- GPU

fn gpu(ui: &mut Ui, v: &View) {
    let p = palette(ui);
    v.notes(ui, "gpu");
    if v.s.gpus.is_empty() {
        card(ui, |ui| empty_state(ui, Icon::Gpu, "No graphics adapters found", "No GPU reported utilisation or sensor data on this system."));
        return;
    }
    for g in &v.s.gpus {
        card(ui, |ui| {
            let kind = if g.integrated { "Integrated" } else { "Discrete" };
            let trailing = match &g.driver {
                Some(d) => format!("{} · {kind} · driver {d}", g.vendor.label()),
                None => format!("{} · {kind}", g.vendor.label()),
            };
            card_header(ui, Icon::Gpu, &g.name, Some(&trailing));
            let mut stats = vec![("Utilization", g.usage.map(f::percent).unwrap_or("—".into()), g.usage.map(Level::for_load))];
            if let Some(t) = g.temperature {
                stats.push(("Temperature", v.temp(t), Some(v.temp_level(t))));
            }
            if let Some(c) = g.core_mhz {
                let max = g.core_max_mhz.map(|m| format!(" / {}", f::mhz(m))).unwrap_or_default();
                stats.push(("Core clock", format!("{}{max}", f::mhz(c)), None));
            }
            if let Some(c) = g.memory_mhz {
                stats.push(("Memory clock", f::mhz(c), None));
            }
            if let Some(w) = g.power_w {
                let lim = g.power_limit_w.map(|l| format!(" / {}", f::watts(l))).unwrap_or_default();
                stats.push(("Power", format!("{}{lim}", f::watts(w)), None));
            }
            if let Some(r) = g.fan_rpm {
                stats.push(("Fan", format!("{r} RPM"), None));
            } else if let Some(pc) = g.fan_pct {
                stats.push(("Fan", format!("{pc:.0}%"), None));
            }
            stat_row(ui, &stats);
            ui.add_space(6.0);
            v.chart(vec![Line { name: "Utilization", series: v.h.get(&format!("gpu.{}.usage", g.key)), color: p.series[0] }], Scale::Fixed(0.0, 100.0), 150.0, &pct_fmt).show(ui);
            ui.add_space(10.0);

            let engines: Vec<&(String, f32)> = g.engines.iter().filter(|e| !matches!(e.0.split_whitespace().next(), Some("Timer" | "Security"))).collect();
            let mut blocks: Vec<Box<dyn Fn(&mut Ui) + '_>> = Vec::new();
            if !engines.is_empty() {
                blocks.push(Box::new(|ui| {
                    section_title(ui, "Engines");
                    for (name, val) in &engines {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(name).color(p.text_secondary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(f::percent(*val)).strong().color(p.text));
                            });
                        });
                        meter(ui, val / 100.0, p.series[0], 6.0);
                    }
                }));
            }
            blocks.push(Box::new(|ui| {
                section_title(ui, "Memory");
                let row = |ui: &mut Ui, label: &str, used: Option<u64>, total: Option<u64>, tip: &str| {
                    let Some(u) = used else { return };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).color(p.text_secondary)).on_hover_text(tip);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let t = total.map(|t| format!(" / {}", f::bytes_u(t))).unwrap_or_default();
                            ui.label(RichText::new(format!("{}{t}", f::bytes_u(u))).strong().color(p.text));
                        });
                    });
                    meter(ui, total.map(|t| u as f32 / t.max(1) as f32).unwrap_or(0.0), p.series[0], 6.0);
                };
                row(ui, if g.integrated { "Reserved (dedicated)" } else { "Dedicated" }, g.dedicated_used, g.dedicated_total, "Video memory that belongs to the GPU");
                row(ui, "Shared", g.shared_used, g.shared_total, "System memory the GPU borrows when needed");
                if g.dedicated_used.is_none() && g.shared_used.is_none() {
                    ui.label(RichText::new("Not reported by this driver").color(p.text_muted));
                }
            }));
            let n = blocks.len();
            responsive_columns(ui, n, 300.0, |ui, i| blocks[i](ui));
            if v.h.get(&format!("gpu.{}.temp", g.key)).is_some() {
                let unit = v.settings.temp_unit;
                let t_fmt = move |x: f64| f::temp(x as f32, unit);
                section_title(ui, "Temperature");
                v.chart(vec![Line { name: "Temperature", series: v.h.get(&format!("gpu.{}.temp", g.key)), color: p.series[1] }], Scale::AutoPadded { min_span: 10.0 }, 110.0, &t_fmt).show(ui);
            }
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Source: {}", g.source)).small().color(p.text_muted));
        });
        ui.add_space(12.0);
    }
}

// ------------------------------------------------------------------- Storage

fn storage(ui: &mut Ui, v: &View) {
    let p = palette(ui);
    if v.s.disks.is_empty() {
        card(ui, |ui| empty_state(ui, Icon::Storage, "No volumes found", "No mounted volumes could be read."));
    }
    let rate_fmt = |x: f64| f::byte_rate(x);
    responsive_columns(ui, v.s.disks.len(), 420.0, |ui, i| {
        let d = &v.s.disks[i];
        card(ui, |ui| {
            let kind = if d.removable { "Removable" } else { "Fixed" };
            card_header(ui, Icon::Storage, &d.name, Some(&format!("{} · {kind}", d.fs)));
            let used = d.total.saturating_sub(d.free);
            let frac = used as f32 / d.total.max(1) as f32;
            let lvl = capacity_level(frac);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} used", f::bytes_u(used))).strong().color(p.text));
                ui.label(RichText::new(format!("of {} · {} free", f::bytes_u(d.total), f::bytes_u(d.free))).color(p.text_secondary));
                if lvl != Level::Normal {
                    level_pill(ui, lvl);
                }
            });
            meter(ui, frac, lvl.bar(p), 10.0);
            ui.add_space(8.0);
            if d.read_bps.is_some() || d.write_bps.is_some() {
                ui.horizontal_wrapped(|ui| {
                    legend_item(ui, p.series[0], "Read", &f::byte_rate(d.read_bps.unwrap_or(0.0)));
                    ui.add_space(12.0);
                    legend_item(ui, p.series[1], "Write", &f::byte_rate(d.write_bps.unwrap_or(0.0)));
                    if let Some(b) = d.busy_pct {
                        ui.add_space(12.0);
                        ui.label(RichText::new(format!("Active {}", f::percent(b))).color(p.text_secondary)).on_hover_text("Share of time the volume was busy");
                    }
                });
                let mut c = v.chart(
                    vec![
                        Line { name: "Read", series: v.h.get(&format!("disk.{}.read", d.key)), color: p.series[0] },
                        Line { name: "Write", series: v.h.get(&format!("disk.{}.write", d.key)), color: p.series[1] },
                    ],
                    Scale::AutoFromZero { min_span: 1024.0 * 1024.0 },
                    120.0,
                    &rate_fmt,
                );
                c.fill = false;
                c.show(ui);
            } else {
                ui.label(RichText::new("Throughput is not available for this volume on this platform.").small().color(p.text_muted));
            }
        });
    });

    if !v.s.drives.is_empty() {
        section_title(ui, "Drive health");
        responsive_columns(ui, v.s.drives.len(), 420.0, |ui, i| {
            let d = &v.s.drives[i];
            card(ui, |ui| {
                card_header(ui, Icon::Storage, &d.model, Some(&d.key));
                let health = match d.critical_warning {
                    Some(0) => Some((Level::Normal, "Healthy")),
                    Some(_) => Some((Level::Critical, "Drive reports a warning")),
                    None => None,
                };
                if let Some((lvl, text)) = health {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 5.0, lvl.color(p));
                        ui.label(RichText::new(text).strong().color(p.text));
                    });
                    ui.add_space(6.0);
                }
                let mut stats: Vec<(&str, String, Option<Level>)> = Vec::new();
                if let Some(t) = d.temperature {
                    stats.push(("Temperature", v.temp(t), Some(Level::for_temp(t, 65.0, 75.0))));
                }
                if let Some(u) = d.life_used_pct {
                    let lvl = match u { x if x >= 100 => Level::Critical, x if x >= 90 => Level::High, x if x >= 75 => Level::Elevated, _ => Level::Normal };
                    stats.push(("Life used", format!("{u}%"), Some(lvl)));
                }
                if let Some(s) = d.spare_pct {
                    stats.push(("Spare capacity", format!("{s}%"), None));
                }
                if let Some(h) = d.power_on_hours {
                    stats.push(("Powered on", format!("{} h", f::count(h)), None));
                }
                if let Some(b) = d.bytes_written {
                    stats.push(("Data written", f::bytes(b as f64), None));
                }
                if let Some(b) = d.bytes_read {
                    stats.push(("Data read", f::bytes(b as f64), None));
                }
                if let Some(n) = d.unsafe_shutdowns {
                    stats.push(("Unsafe shutdowns", f::count(n), None));
                }
                if let Some(n) = d.media_errors {
                    stats.push(("Media errors", f::count(n), (n > 0).then_some(Level::High)));
                }
                if stats.is_empty() {
                    ui.label(RichText::new("This drive does not report health data without administrator rights.").color(p.text_muted));
                } else {
                    stat_row(ui, &stats);
                }
            });
        });
    }
}

// ------------------------------------------------------------------- Network

fn network(ui: &mut Ui, v: &View) {
    let p = palette(ui);
    let rate = |x: f64| f::bit_rate(x);
    let (up, down): (Vec<&NetSample>, Vec<&NetSample>) = v.s.networks.iter().partition(|n| n.up);
    if v.s.networks.is_empty() {
        card(ui, |ui| empty_state(ui, Icon::Network, "No network adapters found", "No physical network interfaces were detected."));
        return;
    }
    for n in &up {
        card(ui, |ui| {
            card_header(ui, Icon::Network, &n.name, Some(&n.description));
            let mut stats: Vec<(&str, String, Option<Level>)> = vec![("Download", f::bit_rate(n.rx_bps), None), ("Upload", f::bit_rate(n.tx_bps), None)];
            if let Some(l) = n.link_bps {
                stats.push(("Link speed", f::link_speed(l), None));
            }
            stats.push(("Received", f::bytes_u(n.rx_total), None));
            stats.push(("Sent", f::bytes_u(n.tx_total), None));
            stat_row(ui, &stats);
            ui.horizontal(|ui| {
                legend_item(ui, p.series[0], "Download", "");
                ui.add_space(8.0);
                legend_item(ui, p.series[1], "Upload", "");
            });
            let mut c = v.chart(
                vec![
                    Line { name: "Download", series: v.h.get(&format!("net.{}.rx", n.key)), color: p.series[0] },
                    Line { name: "Upload", series: v.h.get(&format!("net.{}.tx", n.key)), color: p.series[1] },
                ],
                Scale::AutoFromZero { min_span: 125_000.0 },
                150.0,
                &rate,
            );
            c.fill = false;
            c.show(ui);
            ui.add_space(6.0);
            let mut rows = Vec::new();
            if !n.addresses.is_empty() {
                rows.push(("Addresses", n.addresses.join("\n")));
            }
            if let Some(m) = &n.mac {
                rows.push(("Hardware address", m.clone()));
            }
            kv_grid(ui, &format!("net-{}", n.key), &rows);
        });
        ui.add_space(12.0);
    }
    if !down.is_empty() {
        section_title(ui, "Disconnected adapters");
        card(ui, |ui| {
            for n in down {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, p.text_muted);
                    ui.label(RichText::new(&n.name).color(p.text));
                    ui.label(RichText::new(&n.description).color(p.text_muted));
                });
            }
        });
    }
}

// ------------------------------------------------------------------- Sensors

fn search_box(ui: &mut Ui, text: &mut String, hint: &str, id: &str) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        let resp = egui::Frame::new().fill(p.surface).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                icon_label(ui, Icon::Search, 14.0, p.text_muted);
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique(id)).hint_text(hint).frame(egui::Frame::NONE).desired_width(260.0))
            })
            .inner
        });
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            resp.inner.request_focus();
        }
        if resp.inner.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            text.clear();
        }
        if !text.is_empty() && ui.small_button("Clear").clicked() {
            text.clear();
        }
    });
    ui.add_space(10.0);
}

fn sensors(ui: &mut Ui, v: &View, filter: &mut String, reset_stats: &mut bool) {
    let p = palette(ui);
    v.notes(ui, "cpu");
    search_box(ui, filter, "Filter sensors  (Ctrl+F)", "sensor-filter");
    let needle = filter.to_lowercase();
    let matches = |s: &Sensor| needle.is_empty() || s.label.to_lowercase().contains(&needle) || s.group.to_lowercase().contains(&needle);
    let unit = v.settings.temp_unit;
    let cols = [
        col("Sensor", 3.0, 150.0, false),
        col("Source", 3.0, 120.0, false),
        col("Current", 1.6, 110.0, false),
        col("Min", 1.0, 70.0, false),
        col("Max", 1.0, 70.0, false),
        col("Average", 1.0, 70.0, false),
        col("Last minute", 2.0, 110.0, false),
    ];
    let mut any = false;
    for kind in SensorKind::ALL {
        let mut list: Vec<&Sensor> = v.s.sensors.iter().filter(|s| s.kind == kind && matches(s)).collect();
        if list.is_empty() {
            continue;
        }
        list.sort_by(|a, b| natural_cmp(&a.group, &b.group).then_with(|| natural_cmp(&a.label, &b.label)));
        any = true;
        let open = !needle.is_empty() || kind != SensorKind::Clock;
        card(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new(format!("{}  ·  {}", kind.label(), list.len())).strong().color(p.text))
                .id_salt(("sensors", kind))
                .default_open(open)
                .show(ui, |ui| {
                    let widths = column_widths(&cols, ui.available_width(), 12.0);
                    table_row(ui, &widths, 26.0, 0, true, |ui, i| {
                        ui.label(RichText::new(cols[i].title).small().color(p.text_muted));
                    });
                    for (row, s) in list.iter().enumerate() {
                        let key = format!("sensor.{}", s.id);
                        let level = (kind == SensorKind::Temperature).then(|| sensor_level(s, v.settings));
                        let st = v.h.stats(&key);
                        table_row(ui, &widths, 30.0, row, false, |ui, i| match i {
                            0 => {
                                ui.add(egui::Label::new(RichText::new(&s.label).color(p.text)).truncate());
                            }
                            1 => {
                                ui.add(egui::Label::new(RichText::new(&s.group).color(p.text_secondary)).truncate());
                            }
                            2 => {
                                ui.label(RichText::new(f::sensor_value(kind, s.value, unit)).strong().color(p.text));
                                if let Some(l) = level.filter(|l| *l != Level::Normal) {
                                    level_pill(ui, l);
                                }
                            }
                            3 => {
                                ui.label(RichText::new(st.map(|x| f::sensor_value(kind, x.min, unit)).unwrap_or_default()).color(p.text_secondary));
                            }
                            4 => {
                                ui.label(RichText::new(st.map(|x| f::sensor_value(kind, x.max, unit)).unwrap_or_default()).color(p.text_secondary));
                            }
                            5 => {
                                ui.label(RichText::new(st.map(|x| f::sensor_value(kind, x.avg(), unit)).unwrap_or_default()).color(p.text_secondary));
                            }


                            _ => {
                                let color = level.map(|l| l.bar(p)).unwrap_or(p.series[0]);
                                let w = ui.available_width().min(160.0);
                                sparkline(ui, v.h.get(&key), vec2(w, 18.0), color, None, 60.0, v.now);
                            }
                        });
                    }
                });
        });
        ui.add_space(10.0);
    }
    if !any {
        card(ui, |ui| {
            if needle.is_empty() {
                empty_state(ui, Icon::Sensors, "No sensors found", "This system did not expose any hardware sensors.");
            } else {
                empty_state(ui, Icon::Search, "No matching sensors", "Try a different filter.");
            }
        });
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Min, max and average are tracked since OpenTemp started or since the last reset.").small().color(p.text_muted));
        if ui.small_button("Reset min/max").clicked() {
            *reset_stats = true;
        }
    });
}

fn sensor_level(s: &Sensor, settings: &Settings) -> Level {
    let v = s.value as f32;
    match (s.high, s.critical) {
        (_, Some(c)) if v >= c as f32 => Level::Critical,
        (Some(h), _) if v >= h as f32 => Level::High,
        (Some(h), _) if v >= h as f32 - 10.0 => Level::Elevated,
        (None, None) => Level::for_temp(v, settings.warn_temp, settings.hot_temp),
        _ => Level::Normal,
    }
}

// ----------------------------------------------------------------- Processes

fn processes(ui: &mut Ui, v: &View, filter: &mut String, sort: &mut (ProcSort, bool)) {
    let p = palette(ui);
    search_box(ui, filter, "Filter by name or PID  (Ctrl+F)", "proc-filter");
    let needle = filter.to_lowercase();
    let mut list: Vec<&ProcessSample> = v
        .s
        .processes
        .iter()
        .filter(|pr| needle.is_empty() || pr.name.to_lowercase().contains(&needle) || pr.pid.to_string().contains(&needle))
        .collect();
    let disk = |pr: &ProcessSample| pr.read_bps.unwrap_or(0.0) + pr.write_bps.unwrap_or(0.0);
    list.sort_by(|a, b| {
        let o = match sort.0 {
            ProcSort::Name => natural_cmp(&a.name, &b.name),
            ProcSort::Pid => a.pid.cmp(&b.pid),
            ProcSort::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
            ProcSort::Memory => a.memory.cmp(&b.memory),
            ProcSort::Threads => a.threads.cmp(&b.threads),
            ProcSort::Disk => disk(a).total_cmp(&disk(b)),
        };
        if sort.1 { o.reverse() } else { o }
    });
    let shown = list.len().min(200);
    let total = v.s.system.processes.unwrap_or(v.s.processes.len() as u32);
    ui.label(
        RichText::new(format!("Showing {shown} of {} processes (the busiest by CPU and memory are tracked)", f::count(total as u64)))
            .small()
            .color(p.text_muted),
    );
    ui.add_space(6.0);
    let has_io = v.s.processes.iter().any(|p| p.read_bps.is_some());
    let mem_label = if cfg!(windows) { "Memory (private)" } else { "Memory (resident)" };
    let cols = [
        (col("Name", 4.0, 180.0, false), ProcSort::Name),
        (col("PID", 0.6, 60.0, false), ProcSort::Pid),
        (col("CPU", 1.4, 110.0, false), ProcSort::Cpu),
        (col(mem_label, 1.0, 120.0, false), ProcSort::Memory),
        (col("Threads", 0.6, 60.0, false), ProcSort::Threads),
        (col(if has_io { "Disk" } else { "" }, 1.0, 90.0, false), ProcSort::Disk),
    ];
    let specs: Vec<Col> = cols.iter().map(|(c, _)| col(c.title, c.weight, c.min, c.right)).collect();
    card(ui, |ui| {
        let widths = column_widths(&specs, ui.available_width(), 12.0);
        table_row(ui, &widths, 28.0, 0, true, |ui, i| {
            let (c, key) = &cols[i];
            if c.title.is_empty() {
                return;
            }
            let active = sort.0 == *key;
            let arrow = if active { if sort.1 { "  ▼" } else { "  ▲" } } else { "" };
            let txt = RichText::new(format!("{}{arrow}", c.title)).small().color(if active { p.text } else { p.text_muted });
            if ui.add(egui::Button::new(txt).frame(false)).on_hover_text("Sort by this column").clicked() {
                if active {
                    sort.1 = !sort.1;
                } else {
                    *sort = (*key, !matches!(key, ProcSort::Name | ProcSort::Pid));
                }
            }
        });
        let cores = v.info.cpu.logical_cores.max(1) as f32;
        for (row, pr) in list.iter().take(shown).enumerate() {
            table_row(ui, &widths, 28.0, row, false, |ui, i| match i {
                0 => {
                    ui.add(egui::Label::new(RichText::new(&pr.name).color(p.text)).truncate());
                }
                1 => {
                    ui.label(RichText::new(pr.pid.to_string()).color(p.text_secondary));
                }
                2 => {
                    ui.label(RichText::new(format!("{:.1}%", pr.cpu_pct)).color(p.text)).on_hover_text(format!(
                        "{:.1}% of the whole processor, about {:.1} of {} threads",
                        pr.cpu_pct,
                        pr.cpu_pct / 100.0 * cores,
                        cores as u32
                    ));
                    let w = (ui.available_width() - 4.0).clamp(0.0, 60.0);
                    let (r, _) = ui.allocate_exact_size(vec2(w, 5.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, p.track);
                    let fw = r.width() * (pr.cpu_pct / 100.0).clamp(0.0, 1.0);
                    if fw > 0.5 {
                        ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(fw.max(2.0), r.height())), 2.0, Level::for_load(pr.cpu_pct).bar(p));
                    }
                }
                3 => {
                    ui.label(RichText::new(f::bytes_u(pr.memory)).color(p.text));
                }
                4 => {
                    ui.label(RichText::new(pr.threads.to_string()).color(p.text_secondary));
                }
                _ => {
                    if has_io {
                        let d = disk(pr);
                        ui.label(RichText::new(if d > 0.0 { f::byte_rate(d) } else { "—".into() }).color(p.text_secondary));
                    }
                }
            });
        }
        if list.is_empty() {
            empty_state(ui, Icon::Search, "No matching processes", "Try a different filter.");
        }
    });
}

// ------------------------------------------------------------------ Settings

pub fn settings(ui: &mut Ui, app: &mut App) {
    let p = palette(ui);
    let s = &mut app.settings;
    let row = |ui: &mut Ui, label: &str, help: &str, add: &mut dyn FnMut(&mut Ui)| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(vec2(240.0, 32.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_width(240.0);
                ui.label(RichText::new(label).color(p.text));
                if !help.is_empty() {
                    ui.label(RichText::new(help).small().color(p.text_muted));
                }
            });
            add(ui);
        });
        ui.add_space(8.0);
    };

    card(ui, |ui| {
        card_header(ui, Icon::Overview, "Appearance", None);
        row(ui, "Theme", "Follow the system or pick one", &mut |ui| {
            ui.selectable_value(&mut s.theme, ThemeChoice::System, "System");
            ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
            ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
        });
        row(ui, "Interface size", "Scales all text and controls", &mut |ui| {
            ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0).step_by(0.05).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
        });
        row(ui, "Temperature unit", "", &mut |ui| {
            ui.selectable_value(&mut s.temp_unit, TempUnit::Celsius, "Celsius (°C)");
            ui.selectable_value(&mut s.temp_unit, TempUnit::Fahrenheit, "Fahrenheit (°F)");
        });
        row(ui, "Per-core clocks", "Show each thread's clock on the Processor page", &mut |ui| {
            ui.checkbox(&mut s.show_per_core_clock, "Show");
        });
    });
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Sensors, "Sampling", None);
        row(ui, "Update interval", "Faster updates use slightly more CPU", &mut |ui| {
            egui::ComboBox::from_id_salt("interval").selected_text(super::interval_label(s.interval_ms)).show_ui(ui, |ui| {
                for ms in Settings::INTERVALS_MS {
                    ui.selectable_value(&mut s.interval_ms, ms, super::interval_label(ms));
                }
            });
        });
        row(ui, "History length", "How far back the charts reach", &mut |ui| {
            egui::ComboBox::from_id_salt("history").selected_text(history_label(s.history_secs)).show_ui(ui, |ui| {
                for secs in Settings::HISTORY_SECS {
                    ui.selectable_value(&mut s.history_secs, secs, history_label(secs));
                }
            });
        });
    });
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Sensors, "Temperature thresholds", None);
        ui.label(RichText::new("Used for sensors that do not declare their own limits. Readings are labelled Elevated within 10° of the warning level.").small().color(p.text_muted));
        ui.add_space(6.0);
        let unit = s.temp_unit;
        let fmt = move |v: f64, _: std::ops::RangeInclusive<usize>| f::temp(v as f32, unit);
        row(ui, "Warning", "", &mut |ui| {
            ui.add(egui::Slider::new(&mut s.warn_temp, 40.0..=110.0).step_by(1.0).custom_formatter(fmt));
        });
        if s.hot_temp < s.warn_temp {
            s.hot_temp = s.warn_temp;
        }
        let lo = s.warn_temp;
        row(ui, "Critical", "", &mut |ui| {
            ui.add(egui::Slider::new(&mut s.hot_temp, lo..=120.0).step_by(1.0).custom_formatter(fmt));
        });
    });
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Storage, "Logging", None);
        row(ui, "Log readings to CSV", "Appends one row per sample", &mut |ui| {
            ui.checkbox(&mut s.log_enabled, "Enabled");
        });
        row(ui, "File", "Columns are fixed when logging starts", &mut |ui| {
            ui.add(egui::TextEdit::singleline(&mut s.log_path).desired_width(420.0));
        });
    });
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Info, "Keyboard shortcuts", None);
        kv_grid(ui, "keys", &[
            ("Ctrl+1 … Ctrl+9", "Switch page".into()),
            ("Ctrl+P", "Pause or resume updates".into()),
            ("Ctrl+F", "Filter sensors or processes".into()),
            ("Ctrl+,", "Open settings".into()),
            ("Tab / Enter", "Move between and activate controls".into()),
        ]);
    });
    ui.add_space(12.0);

    card(ui, |ui| {
        card_header(ui, Icon::Logo, "About", None);
        kv_grid(ui, "about", &[
            ("Version", format!("{} {}", crate::APP_NAME, env!("CARGO_PKG_VERSION"))),
            ("Privacy", "OpenTemp reads local hardware counters only. It makes no network connections and collects no telemetry.".into()),
            ("Data sources", data_sources().into()),
        ]);
        ui.add_space(6.0);
        if ui.button("Restore default settings").clicked() {
            *s = Settings::default();
        }
    });
}

fn history_label(secs: u32) -> String {
    if secs >= 3600 { format!("{} hour", secs / 3600) } else if secs >= 60 { format!("{} minutes", secs / 60) } else { format!("{secs} seconds") }
}

fn data_sources() -> &'static str {
    if cfg!(windows) {
        "Windows performance counters (PDH), DXGI and D3DKMT for GPUs, IP Helper, the storage driver's NVMe health log, ACPI thermal zones, RAPL energy meters, and NVML for NVIDIA GPUs. No administrator rights or kernel driver are used."
    } else if cfg!(target_os = "linux") {
        "/proc, sysfs hwmon, cpufreq, the amdgpu/i915 DRM interfaces, power_supply, and NVML for NVIDIA GPUs."
    } else {
        "Mach host statistics, sysctl, getifaddrs, getmntinfo and libproc."
    }
}

