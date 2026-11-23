//! Plain data types shared between the collectors and the UI.
//!
//! Every reading that a platform may not be able to provide is an `Option`, so
//! the UI can tell "zero" apart from "unknown" and explain the gap instead of
//! showing a misleading number.

// macOS does not use every shared type yet (no GPU or sensor backends there).
#![cfg_attr(target_os = "macos", allow(dead_code))]

use serde::Serialize;

/// Facts about the machine that do not change while the app runs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StaticInfo {
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub hostname: String,
    pub arch: String,
    pub cpu: CpuInfo,
    pub memory_total: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CpuInfo {
    pub model: String,
    pub vendor: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    pub sockets: usize,
    pub base_mhz: Option<f64>,
    pub max_mhz: Option<f64>,
    pub l1_bytes: Option<u64>,
    pub l2_bytes: Option<u64>,
    pub l3_bytes: Option<u64>,
    /// One entry per logical processor; `None` when the platform does not say.
    pub core_classes: Vec<Option<u8>>,
    pub features: Vec<String>,
}

/// One sampling pass over every collector.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    /// Seconds since the sampler started.
    pub t: f64,
    pub cpu: CpuSample,
    pub memory: MemorySample,
    pub gpus: Vec<GpuSample>,
    pub disks: Vec<DiskSample>,
    pub drives: Vec<DriveHealth>,
    pub networks: Vec<NetSample>,
    pub sensors: Vec<Sensor>,
    pub battery: Option<Battery>,
    pub processes: Vec<ProcessSample>,
    pub system: SystemSample,
    /// Human-readable notes about sources that were unavailable, shown in the UI.
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CpuSample {
    /// 0..=100
    pub usage: Option<f32>,
    pub per_core: Vec<CoreSample>,
    pub avg_mhz: Option<f64>,
    pub max_core_mhz: Option<f64>,
    /// Best available CPU temperature (package, Tctl, or a thermal zone).
    pub temperature: Option<f32>,
    pub temperature_source: Option<String>,
    pub package_power_w: Option<f32>,
    pub load_avg: Option<[f64; 3]>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CoreSample {
    pub usage: f32,
    pub mhz: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SystemSample {
    pub uptime_secs: Option<u64>,
    pub processes: Option<u32>,
    pub threads: Option<u32>,
    pub handles: Option<u32>,
    pub context_switches_per_sec: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MemorySample {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub cached: Option<u64>,
    pub committed: Option<u64>,
    pub commit_limit: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_used: Option<u64>,
}

impl MemorySample {
    pub fn used_pct(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.used as f64 / self.total as f64 * 100.0) as f32 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Microsoft,
    #[default]
    Other,
}

impl GpuVendor {
    pub fn from_pci_id(id: u32) -> Self {
        match id {
            0x10de => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            0x106b => Self::Apple,
            0x1414 => Self::Microsoft,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nvidia => "NVIDIA",
            Self::Amd => "AMD",
            Self::Intel => "Intel",
            Self::Apple => "Apple",
            Self::Microsoft => "Microsoft",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuSample {
    /// Stable key used for history (e.g. a LUID or a PCI path).
    pub key: String,
    pub name: String,
    pub vendor: GpuVendor,
    pub integrated: bool,
    /// 0..=100, the busiest engine (matches what Task Manager reports).
    pub usage: Option<f32>,
    /// Per-engine utilisation, 0..=100.
    pub engines: Vec<(String, f32)>,
    pub dedicated_used: Option<u64>,
    pub dedicated_total: Option<u64>,
    pub shared_used: Option<u64>,
    pub shared_total: Option<u64>,
    pub temperature: Option<f32>,
    pub core_mhz: Option<f64>,
    pub core_max_mhz: Option<f64>,
    pub memory_mhz: Option<f64>,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub fan_pct: Option<f32>,
    pub driver: Option<String>,
    /// Which APIs supplied the readings, e.g. "PDH + D3DKMT".
    pub source: String,
}

impl GpuSample {
    /// Copy every reading that `self` lacks from `other`.
    #[cfg_attr(not(windows), allow(dead_code))] // only Windows merges two GPU sources
    pub fn fill_from(&mut self, other: &GpuSample) {
        macro_rules! fill {
            ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = other.$f.clone(); } )* };
        }
        fill!(
            usage,
            dedicated_used,
            dedicated_total,
            shared_used,
            shared_total,
            temperature,
            core_mhz,
            core_max_mhz,
            memory_mhz,
            power_w,
            power_limit_w,
            fan_rpm,
            fan_pct,
            driver
        );
        if self.engines.is_empty() {
            self.engines = other.engines.clone();
        }
        if !other.source.is_empty() && !self.source.contains(&other.source) {
            self.source = format!("{} + {}", self.source, other.source);
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DiskSample {
    pub key: String,
    pub name: String,
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
    /// 0..=100
    pub busy_pct: Option<f32>,
}

/// Health of a physical drive, from the NVMe SMART / health log where available.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DriveHealth {
    pub key: String,
    pub model: String,
    pub temperature: Option<f32>,
    /// Vendor estimate of life used, 0..=255 (can exceed 100).
    pub life_used_pct: Option<u8>,
    pub spare_pct: Option<u8>,
    pub power_on_hours: Option<u64>,
    pub bytes_read: Option<u64>,
    pub bytes_written: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    pub media_errors: Option<u64>,
    /// Critical warning bits from the health log; non-zero means the drive reports a problem.
    pub critical_warning: Option<u8>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct NetSample {
    pub key: String,
    pub name: String,
    pub description: String,
    pub up: bool,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub link_bps: Option<u64>,
    pub addresses: Vec<String>,
    pub mac: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum SensorKind {
    Temperature,
    Fan,
    Voltage,
    Current,
    Power,
    Clock,
    Load,
}

impl SensorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Temperature => "Temperatures",
            Self::Fan => "Fans",
            Self::Voltage => "Voltages",
            Self::Current => "Currents",
            Self::Power => "Power",
            Self::Clock => "Clocks",
            Self::Load => "Load",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Fan => "RPM",
            Self::Voltage => "V",
            Self::Current => "A",
            Self::Power => "W",
            Self::Clock => "MHz",
            Self::Load => "%",
        }
    }

    pub const ALL: [SensorKind; 7] =
        [Self::Temperature, Self::Power, Self::Clock, Self::Load, Self::Fan, Self::Voltage, Self::Current];
}

/// A single named reading from a hardware sensor. Temperatures are always °C.
#[derive(Clone, Debug, Serialize)]
pub struct Sensor {
    /// Stable id, unique within a snapshot.
    pub id: String,
    /// The chip or subsystem that reported it, e.g. "k10temp" or "ACPI".
    pub group: String,
    pub label: String,
    pub kind: SensorKind,
    pub value: f64,
    /// Vendor-declared warning / critical thresholds, when known.
    pub high: Option<f64>,
    pub critical: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Battery {
    pub percent: Option<f32>,
    pub charging: bool,
    pub on_ac: bool,
    pub seconds_left: Option<u64>,
    pub power_w: Option<f32>,
    pub health_pct: Option<f32>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    /// Share of the whole machine, 0..=100.
    pub cpu_pct: f32,
    pub memory: u64,
    pub threads: u32,
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Note {
    pub area: &'static str,
    pub text: String,
}

impl Note {
    pub fn new(area: &'static str, text: impl Into<String>) -> Self {
        Self { area, text: text.into() }
    }
}
