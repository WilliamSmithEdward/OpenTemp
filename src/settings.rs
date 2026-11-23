//! User preferences, persisted by eframe between runs.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub temp_unit: TempUnit,
    pub interval_ms: u64,
    pub history_secs: u32,
    /// Temperature (°C) at which readings turn amber / red when the sensor has no own limits.
    pub warn_temp: f32,
    pub hot_temp: f32,
    pub ui_scale: f32,
    pub log_enabled: bool,
    pub log_path: String,
    pub show_per_core_clock: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            temp_unit: TempUnit::Celsius,
            interval_ms: 1000,
            history_secs: 300,
            warn_temp: 75.0,
            hot_temp: 90.0,
            ui_scale: 1.0,
            log_enabled: false,
            log_path: default_log_path(),
            show_per_core_clock: true,
        }
    }
}

fn default_log_path() -> String {
    let base = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("opentemp-log.csv").to_string_lossy().into_owned()
}

impl Settings {
    pub const INTERVALS_MS: [u64; 5] = [250, 500, 1000, 2000, 5000];
    pub const HISTORY_SECS: [u32; 5] = [60, 300, 600, 1800, 3600];

    /// Clamp values from an older or hand-edited settings file.
    pub fn sanitized(mut self) -> Self {
        self.interval_ms = self.interval_ms.clamp(250, 10_000);
        self.history_secs = self.history_secs.clamp(30, 3600);
        self.ui_scale = self.ui_scale.clamp(0.75, 2.0);
        if !(30.0..=120.0).contains(&self.warn_temp) {
            self.warn_temp = 75.0;
        }
        if !(self.warn_temp..=130.0).contains(&self.hot_temp) {
            self.hot_temp = (self.warn_temp + 15.0).min(130.0);
        }
        self
    }
}
