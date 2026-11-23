//! Human-friendly number formatting. Units are always shown so a value is
//! never ambiguous.

use crate::settings::TempUnit;

/// Bytes with binary multiples, labelled the way most OS tools do (1 GB = 1024³ bytes).
pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

pub fn bytes_u(b: u64) -> String {
    bytes(b as f64)
}

/// Disk throughput, bytes per second.
pub fn byte_rate(bps: f64) -> String {
    format!("{}/s", bytes(bps))
}

/// Network throughput: networks are specified in bits per second (decimal).
pub fn bit_rate(bytes_per_sec: f64) -> String {
    let bits = bytes_per_sec.max(0.0) * 8.0;
    let (v, u) = if bits >= 1e9 {
        (bits / 1e9, "Gbps")
    } else if bits >= 1e6 {
        (bits / 1e6, "Mbps")
    } else if bits >= 1e3 {
        (bits / 1e3, "Kbps")
    } else {
        (bits, "bps")
    };
    if u == "bps" || v >= 100.0 { format!("{v:.0} {u}") } else { format!("{v:.1} {u}") }
}

pub fn link_speed(bits_per_sec: u64) -> String {
    let b = bits_per_sec as f64;
    if b >= 1e9 {
        let g = b / 1e9;
        if g.fract().abs() < 0.05 { format!("{g:.0} Gbps") } else { format!("{g:.1} Gbps") }
    } else {
        format!("{:.0} Mbps", b / 1e6)
    }
}

pub fn mhz(m: f64) -> String {
    if m >= 1000.0 { format!("{:.2} GHz", m / 1000.0) } else { format!("{m:.0} MHz") }
}

pub fn percent(p: f32) -> String {
    if p < 10.0 && p > 0.0 { format!("{p:.1}%") } else { format!("{p:.0}%") }
}

pub fn temp(c: f32, unit: TempUnit) -> String {
    match unit {
        TempUnit::Celsius => format!("{c:.0} °C"),
        TempUnit::Fahrenheit => format!("{:.0} °F", c * 9.0 / 5.0 + 32.0),
    }
}

pub fn temp_precise(c: f64, unit: TempUnit) -> String {
    match unit {
        TempUnit::Celsius => format!("{c:.1} °C"),
        TempUnit::Fahrenheit => format!("{:.1} °F", c * 9.0 / 5.0 + 32.0),
    }
}

pub fn watts(w: f32) -> String {
    if w < 10.0 { format!("{w:.2} W") } else { format!("{w:.1} W") }
}

pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A sensor value with its unit, honouring the temperature unit setting.
pub fn sensor_value(kind: crate::model::SensorKind, v: f64, unit: TempUnit) -> String {
    use crate::model::SensorKind::*;
    match kind {
        Temperature => temp_precise(v, unit),
        Fan => format!("{v:.0} RPM"),
        Voltage => format!("{v:.3} V"),
        Current => format!("{v:.2} A"),
        Power => watts(v as f32),
        Clock => mhz(v),
        Load => format!("{v:.1}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1536.0), "1.50 KB");
        assert_eq!(bytes(16.0 * 1024.0 * 1024.0 * 1024.0), "16.0 GB");
        assert_eq!(bit_rate(125_000.0), "1.0 Mbps");
        assert_eq!(mhz(3450.0), "3.45 GHz");
        assert_eq!(mhz(800.0), "800 MHz");
        assert_eq!(temp(50.0, TempUnit::Fahrenheit), "122 °F");
        assert_eq!(duration(90061), "1d 1h 1m");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(link_speed(2_500_000_000), "2.5 Gbps");
    }
}
