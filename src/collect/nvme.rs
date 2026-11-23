//! Decoder for the NVMe "SMART / Health Information" log page (log id 0x02),
//! as laid out in the NVM Express base specification.

#![allow(dead_code)]

#[derive(Debug, Default, PartialEq)]
pub struct NvmeHealth {
    pub critical_warning: u8,
    pub composite_c: Option<f32>,
    pub spare_pct: u8,
    pub life_used_pct: u8,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub power_on_hours: u64,
    pub unsafe_shutdowns: u64,
    pub media_errors: u64,
    /// Up to eight extra sensors, °C, index = sensor number - 1.
    pub sensors_c: Vec<(usize, f32)>,
}

fn kelvin(raw: u16) -> Option<f32> {
    // 0 means "not implemented"; reject values outside a physically sane range.
    let c = raw as f32 - 273.15;
    (raw != 0 && (-40.0..=150.0).contains(&c)).then_some(c)
}

fn u128_le(b: &[u8]) -> u128 {
    let mut a = [0u8; 16];
    a.copy_from_slice(&b[..16]);
    u128::from_le_bytes(a)
}

pub fn parse_health_log(log: &[u8]) -> Option<NvmeHealth> {
    if log.len() < 216 {
        return None;
    }
    // "Data units" are thousands of 512-byte blocks.
    let units = |at: usize| (u128_le(&log[at..]).saturating_mul(512_000)).min(u64::MAX as u128) as u64;
    let count = |at: usize| u128_le(&log[at..]).min(u64::MAX as u128) as u64;
    Some(NvmeHealth {
        critical_warning: log[0],
        composite_c: kelvin(u16::from_le_bytes([log[1], log[2]])),
        spare_pct: log[3],
        life_used_pct: log[5],
        bytes_read: units(32),
        bytes_written: units(48),
        power_on_hours: count(128),
        unsafe_shutdowns: count(160),
        media_errors: count(176),
        sensors_c: (0..8)
            .filter_map(|i| kelvin(u16::from_le_bytes([log[200 + i * 2], log[201 + i * 2]])).map(|c| (i, c)))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_health_log() {
        let mut log = vec![0u8; 512];
        log[1..3].copy_from_slice(&309u16.to_le_bytes());
        log[3] = 100;
        log[5] = 3;
        log[48..64].copy_from_slice(&1000u128.to_le_bytes());
        log[128..144].copy_from_slice(&4321u128.to_le_bytes());
        log[202..204].copy_from_slice(&345u16.to_le_bytes());
        let h = parse_health_log(&log).unwrap();
        assert!((h.composite_c.unwrap() - 35.85).abs() < 0.01);
        assert_eq!((h.spare_pct, h.life_used_pct), (100, 3));
        assert_eq!(h.bytes_written, 512_000_000);
        assert_eq!(h.power_on_hours, 4321);
        assert_eq!(h.sensors_c.len(), 1);
        assert_eq!(h.sensors_c[0].0, 1);
        assert!(parse_health_log(&log[..100]).is_none());
    }
}
