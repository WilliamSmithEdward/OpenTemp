//! Time series buffers and running statistics for charts and min/max columns.

use std::collections::{HashMap, VecDeque};

use crate::model::{SensorKind, Snapshot};

#[derive(Default, Clone)]
pub struct Series {
    pub points: VecDeque<(f64, f32)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    sum: f64,
    n: u64,
}

impl Stats {
    fn new(v: f64) -> Self {
        Self { min: v, max: v, sum: v, n: 1 }
    }
    fn push(&mut self, v: f64) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v;
        self.n += 1;
    }
    pub fn avg(&self) -> f64 {
        self.sum / self.n.max(1) as f64
    }
}

pub struct History {
    pub window_secs: f64,
    series: HashMap<String, Series>,
    stats: HashMap<String, Stats>,
}

impl History {
    pub fn new(window_secs: f64) -> Self {
        Self { window_secs, series: HashMap::new(), stats: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&Series> {
        self.series.get(key)
    }

    pub fn stats(&self, key: &str) -> Option<&Stats> {
        self.stats.get(key)
    }

    pub fn reset_stats(&mut self) {
        self.stats.clear();
    }

    pub fn push(&mut self, key: impl Into<String>, t: f64, v: f64) {
        if !v.is_finite() {
            return;
        }
        let key = key.into();
        self.stats.entry(key.clone()).and_modify(|s| s.push(v)).or_insert_with(|| Stats::new(v));
        let s = self.series.entry(key).or_default();
        s.points.push_back((t, v as f32));
        let cutoff = t - self.window_secs;
        while s.points.front().is_some_and(|p| p.0 < cutoff) {
            s.points.pop_front();
        }
    }

    /// Drop series that have received no data for a full window (unplugged devices).
    pub fn prune(&mut self, now: f64) {
        let cutoff = now - self.window_secs;
        self.series.retain(|_, s| s.points.back().is_some_and(|p| p.0 >= cutoff));
    }

    /// Record every chartable value of a snapshot under stable keys.
    pub fn record(&mut self, s: &Snapshot) {
        let t = s.t;
        if let Some(u) = s.cpu.usage {
            self.push("cpu.usage", t, u as f64);
        }
        for (i, c) in s.cpu.per_core.iter().enumerate() {
            self.push(format!("cpu.core{i}"), t, c.usage as f64);
        }
        if let Some(m) = s.cpu.avg_mhz {
            self.push("cpu.mhz", t, m);
        }
        if let Some(v) = s.cpu.temperature {
            self.push("cpu.temp", t, v as f64);
        }
        if let Some(v) = s.cpu.package_power_w {
            self.push("cpu.power", t, v as f64);
        }
        if s.memory.total > 0 {
            self.push("mem.used_pct", t, s.memory.used_pct() as f64);
            self.push("mem.used", t, s.memory.used as f64);
        }
        if let (Some(u), Some(tot)) = (s.memory.swap_used, s.memory.swap_total)
            && tot > 0
        {
            self.push("mem.swap_pct", t, u as f64 / tot as f64 * 100.0);
        }
        for g in &s.gpus {
            let k = &g.key;
            if let Some(v) = g.usage {
                self.push(format!("gpu.{k}.usage"), t, v as f64);
            }
            for (name, v) in &g.engines {
                self.push(format!("gpu.{k}.engine.{name}"), t, *v as f64);
            }
            if let Some(v) = g.dedicated_used {
                self.push(format!("gpu.{k}.dedicated"), t, v as f64);
            }
            if let Some(v) = g.shared_used {
                self.push(format!("gpu.{k}.shared"), t, v as f64);
            }
            if let Some(v) = g.temperature {
                self.push(format!("gpu.{k}.temp"), t, v as f64);
            }
            if let Some(v) = g.core_mhz {
                self.push(format!("gpu.{k}.mhz"), t, v);
            }
            if let Some(v) = g.power_w {
                self.push(format!("gpu.{k}.power"), t, v as f64);
            }
        }
        for d in &s.disks {
            if let Some(v) = d.read_bps {
                self.push(format!("disk.{}.read", d.key), t, v);
            }
            if let Some(v) = d.write_bps {
                self.push(format!("disk.{}.write", d.key), t, v);
            }
            if let Some(v) = d.busy_pct {
                self.push(format!("disk.{}.busy", d.key), t, v as f64);
            }
        }
        let (mut rx, mut tx) = (0.0, 0.0);
        for n in &s.networks {
            self.push(format!("net.{}.rx", n.key), t, n.rx_bps);
            self.push(format!("net.{}.tx", n.key), t, n.tx_bps);
            rx += n.rx_bps;
            tx += n.tx_bps;
        }
        self.push("net.rx", t, rx);
        self.push("net.tx", t, tx);
        for sensor in &s.sensors {
            self.push(format!("sensor.{}", sensor.id), t, sensor.value);
        }
        if let Some(b) = &s.battery
            && let Some(p) = b.percent
        {
            self.push("battery.pct", t, p as f64);
        }
        self.prune(t);
    }
}

/// Turn derived readings (GPU temperatures, clocks, power) into sensors so the
/// Sensors page lists everything in one place.
pub fn derive_sensors(s: &mut Snapshot) {
    use crate::model::Sensor;
    let mut extra = Vec::new();
    let mut add = |id: String, group: &str, label: &str, kind: SensorKind, value: Option<f64>| {
        if let Some(value) = value {
            extra.push(Sensor {
                id,
                group: group.to_string(),
                label: label.to_string(),
                kind,
                value,
                high: None,
                critical: None,
            });
        }
    };
    if !s.cpu.per_core.is_empty() {
        add("cpu.load".into(), "CPU", "Total load", SensorKind::Load, s.cpu.usage.map(f64::from));
        add("cpu.clock.avg".into(), "CPU", "Effective clock (average)", SensorKind::Clock, s.cpu.avg_mhz);
        for (i, c) in s.cpu.per_core.iter().enumerate() {
            add(format!("cpu.clock.{i}"), "CPU", &format!("CPU {i} clock"), SensorKind::Clock, c.mhz);
        }
    }
    for g in &s.gpus {
        let k = &g.key;
        add(format!("gpu.{k}.temp"), &g.name, "GPU temperature", SensorKind::Temperature, g.temperature.map(f64::from));
        add(format!("gpu.{k}.load"), &g.name, "GPU load", SensorKind::Load, g.usage.map(f64::from));
        add(format!("gpu.{k}.core"), &g.name, "GPU core clock", SensorKind::Clock, g.core_mhz);
        add(format!("gpu.{k}.mem"), &g.name, "GPU memory clock", SensorKind::Clock, g.memory_mhz);
        add(format!("gpu.{k}.power"), &g.name, "GPU power", SensorKind::Power, g.power_w.map(f64::from));
        add(format!("gpu.{k}.fan"), &g.name, "GPU fan", SensorKind::Fan, g.fan_rpm.map(f64::from));
    }
    if let Some(b) = &s.battery {
        add("battery.charge".into(), "Battery", "Charge level", SensorKind::Load, b.percent.map(f64::from));
        add("battery.power".into(), "Battery", "Battery power", SensorKind::Power, b.power_w.map(f64::from));
    }
    s.sensors.extend(extra);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_drops_old_points_and_keeps_stats() {
        let mut h = History::new(10.0);
        for i in 0..30 {
            h.push("x", i as f64, i as f64);
        }
        let s = h.get("x").unwrap();
        assert_eq!(s.points.front().unwrap().0, 19.0);
        assert_eq!(s.points.back().map(|p| p.1), Some(29.0));
        let st = h.stats("x").unwrap();
        assert_eq!((st.min, st.max), (0.0, 29.0));
        assert!((st.avg() - 14.5).abs() < 1e-9);
    }

    #[test]
    fn non_finite_values_are_ignored() {
        let mut h = History::new(10.0);
        h.push("x", 0.0, f64::NAN);
        assert!(h.get("x").is_none());
    }
}
