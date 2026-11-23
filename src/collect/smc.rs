//! Apple System Management Controller (SMC) sensors.
//!
//! The SMC exposes readings as four-character keys ("TC0P", "F0Ac", …), each
//! with a four-character data type ("sp78", "flt ", "fpe2", …). Decoding and
//! labelling are pure functions compiled on every platform so they are tested
//! everywhere; the IOKit client itself only exists on macOS.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Pack a four-character code into the big-endian u32 the SMC uses.
pub fn fourcc(s: &str) -> u32 {
    let mut b = [b' '; 4];
    for (i, c) in s.bytes().take(4).enumerate() {
        b[i] = c;
    }
    u32::from_be_bytes(b)
}

pub fn fourcc_str(v: u32) -> String {
    v.to_be_bytes().iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect()
}

fn hex_digit(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

/// Decode an SMC value. Fixed-point and integer types are big-endian;
/// `flt ` (used on Apple silicon) is a little-endian IEEE float.
pub fn decode(data_type: u32, bytes: &[u8]) -> Option<f64> {
    let t = data_type.to_be_bytes();
    let be16 = || Some(u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]));
    match &t {
        b"flt " => {
            let v = f32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as f64;
            v.is_finite().then_some(v)
        }
        b"ui8 " => bytes.first().map(|&v| v as f64),
        b"ui16" => be16().map(f64::from),
        b"ui32" => Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as f64),
        b"si8 " => bytes.first().map(|&v| v as i8 as f64),
        b"si16" => be16().map(|v| v as i16 as f64),
        // "spXY": signed 16-bit fixed point; Y (hex) is the number of fraction bits.
        [b's', b'p', _, y] => Some(be16()? as i16 as f64 / f64::from(1u32 << hex_digit(*y)?)),
        // "fpXY": unsigned 16-bit fixed point.
        [b'f', b'p', _, y] => Some(be16()? as f64 / f64::from(1u32 << hex_digit(*y)?)),
        _ => None,
    }
}

/// What a temperature key measures, from its well-known naming scheme.
/// Returns `(category, description)`; unknown keys still get a generic label.
pub fn temperature_label(key: &str) -> (&'static str, String) {
    let b = key.as_bytes();
    if b.len() != 4 || b[0] != b'T' {
        return ("Other", format!("Sensor {key}"));
    }
    let n = |c: u8| (c as char).to_string();
    match (b[1], b[2], b[3]) {
        // Intel Macs
        (b'C', b'0', b'P') => ("CPU", "CPU proximity".into()),
        (b'C', b'0', b'D' | b'E' | b'F') => ("CPU", "CPU die".into()),
        (b'C', b'0', b'H') => ("CPU", "CPU heatsink".into()),
        (b'C', b'X', b'C') => ("CPU", "CPU PECI".into()),
        (b'C', d, b'C') if d.is_ascii_digit() => ("CPU", format!("CPU core {}", n(d))),
        (b'C', _, _) => ("CPU", "CPU".into()),
        (b'G', b'0', b'P') => ("GPU", "GPU proximity".into()),
        (b'G', b'0', b'D') => ("GPU", "GPU die".into()),
        (b'G', _, _) => ("GPU", "GPU".into()),
        // Apple silicon: p = performance cores, e = efficiency cores, g = GPU
        (b'p', _, _) => ("CPU", "Performance cluster".into()),
        (b'e', _, _) => ("CPU", "Efficiency cluster".into()),
        (b'f', _, _) => ("CPU", "CPU".into()),
        (b'g', _, _) => ("GPU", "GPU cluster".into()),
        (b'B', _, _) => ("Battery", "Battery".into()),
        (b'A', _, _) => ("Ambient", "Ambient".into()),
        (b'a', _, _) => ("Ambient", "Airflow".into()),
        (b'H', _, _) => ("Storage", "Drive".into()),
        (b'M', _, _) => ("Memory", "Memory".into()),
        (b'm', _, _) => ("Memory", "Memory".into()),
        (b'N', _, _) => ("Platform", "Platform controller".into()),
        (b's' | b'S', _, _) => ("Enclosure", "Skin / palm rest".into()),
        (b'h', _, _) => ("Platform", "Heatsink".into()),
        (b'W', _, _) => ("Wireless", "Wireless".into()),
        (b'I', _, _) => ("Platform", "Thunderbolt / IO".into()),
        _ => ("Other", "Sensor".into()),
    }
}

/// Power keys whose meaning is well established. Others are left out rather
/// than shown with a guessed name.
pub fn power_label(key: &str) -> Option<&'static str> {
    Some(match key {
        "PSTR" => "System total",
        "PCPT" => "CPU package total",
        "PCPC" => "CPU package cores",
        "PCPG" => "GPU",
        "PC0C" => "CPU core",
        "PDTR" => "DC in",
        "PPBR" => "Battery",
        "PZ0E" => "Platform",
        _ => return None,
    })
}

/// A temperature reading is only trusted inside a physically plausible range;
/// the SMC reports unused sensors as 0, negative values or fixed sentinels.
pub fn plausible_temp(c: f64) -> bool {
    (5.0..=130.0).contains(&c)
}

#[cfg(target_os = "macos")]
pub use client::Smc;

#[cfg(target_os = "macos")]
mod client {
    use std::ffi::{c_char, c_void};

    use super::fourcc;

    type KernReturn = i32;
    type MachPort = u32;

    const KERNEL_INDEX_SMC: u32 = 2;
    const CMD_READ_BYTES: u8 = 5;
    const CMD_READ_INDEX: u8 = 8;
    const CMD_READ_KEYINFO: u8 = 9;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Version {
        major: u8,
        minor: u8,
        build: u8,
        reserved: u8,
        release: u16,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct PLimit {
        version: u16,
        length: u16,
        cpu_limit: u32,
        gpu_limit: u32,
        mem_limit: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct KeyInfo {
        data_size: u32,
        data_type: u32,
        attributes: u8,
    }

    /// The AppleSMC user-client parameter block (80 bytes).
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Param {
        key: u32,
        version: Version,
        plimit: PLimit,
        key_info: KeyInfo,
        result: u8,
        status: u8,
        data8: u8,
        data32: u32,
        bytes: [u8; 32],
    }

    const _: () = assert!(size_of::<Param>() == 80, "SMC parameter block must be 80 bytes");

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        static mach_task_self_: MachPort;
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> MachPort;
        fn IOServiceOpen(service: MachPort, owning_task: MachPort, kind: u32, connect: *mut MachPort) -> KernReturn;
        fn IOServiceClose(connect: MachPort) -> KernReturn;
        fn IOObjectRelease(object: MachPort) -> KernReturn;
        fn IOConnectCallStructMethod(
            connection: MachPort,
            selector: u32,
            input: *const c_void,
            input_size: usize,
            output: *mut c_void,
            output_size: *mut usize,
        ) -> KernReturn;
    }

    pub struct Smc {
        conn: MachPort,
    }

    impl Smc {
        /// Connect to the AppleSMC service. Works without root.
        pub fn open() -> Option<Self> {
            // SAFETY: IOServiceMatching returns a dictionary that IOServiceGetMatchingService consumes.
            unsafe {
                let matching = IOServiceMatching(c"AppleSMC".as_ptr());
                if matching.is_null() {
                    return None;
                }
                // 0 is kIOMainPortDefault.
                let service = IOServiceGetMatchingService(0, matching);
                if service == 0 {
                    return None;
                }
                let mut conn = 0;
                let kr = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
                IOObjectRelease(service);
                (kr == 0 && conn != 0).then_some(Self { conn })
            }
        }

        fn call(&self, input: &Param) -> Option<Param> {
            let mut out = Param::default();
            let mut size = size_of::<Param>();
            // SAFETY: both buffers are 80-byte parameter blocks as the driver expects.
            let kr = unsafe {
                IOConnectCallStructMethod(
                    self.conn,
                    KERNEL_INDEX_SMC,
                    (input as *const Param).cast(),
                    size_of::<Param>(),
                    (&mut out as *mut Param).cast(),
                    &mut size,
                )
            };
            (kr == 0 && out.result == 0).then_some(out)
        }

        /// `(data type, data size)` of a key.
        pub fn key_info(&self, key: u32) -> Option<(u32, u32)> {
            let out = self.call(&Param { key, data8: CMD_READ_KEYINFO, ..Default::default() })?;
            Some((out.key_info.data_type, out.key_info.data_size))
        }

        /// Raw bytes of a key whose size is already known.
        pub fn read(&self, key: u32, size: u32) -> Option<Vec<u8>> {
            let size = size.min(32);
            let input = Param {
                key,
                data8: CMD_READ_BYTES,
                key_info: KeyInfo { data_size: size, ..Default::default() },
                ..Default::default()
            };
            let out = self.call(&input)?;
            Some(out.bytes[..size as usize].to_vec())
        }

        pub fn read_value(&self, key: &str) -> Option<f64> {
            let k = fourcc(key);
            let (ty, size) = self.key_info(k)?;
            super::decode(ty, &self.read(k, size)?)
        }

        /// Every key the SMC knows about.
        pub fn keys(&self) -> Vec<u32> {
            let count = self.read_value("#KEY").unwrap_or(0.0) as u32;
            (0..count.min(8192))
                .filter_map(|i| {
                    self.call(&Param { data8: CMD_READ_INDEX, data32: i, ..Default::default() }).map(|o| o.key)
                })
                .collect()
        }
    }

    impl Drop for Smc {
        fn drop(&mut self) {
            // SAFETY: closing the connection we opened.
            unsafe {
                IOServiceClose(self.conn);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_round_trip() {
        assert_eq!(fourcc("TC0P"), 0x5443_3050);
        assert_eq!(fourcc_str(fourcc("flt")), "flt ");
    }

    #[test]
    fn decodes_common_types() {
        // sp78: 0x2A80 = 42.5 °C
        assert_eq!(decode(fourcc("sp78"), &[0x2A, 0x80]), Some(42.5));
        // sp78 negative
        assert_eq!(decode(fourcc("sp78"), &[0xFF, 0x00]), Some(-1.0));
        // fpe2: 14.2 fixed; 0x1F40 = 8000 / 4 = 2000 RPM
        assert_eq!(decode(fourcc("fpe2"), &[0x1F, 0x40]), Some(2000.0));
        assert_eq!(decode(fourcc("flt "), &48.25f32.to_le_bytes()), Some(48.25));
        assert_eq!(decode(fourcc("ui8 "), &[3]), Some(3.0));
        assert_eq!(decode(fourcc("ui32"), &[0, 0, 4, 0]), Some(1024.0));
        assert_eq!(decode(fourcc("ch8*"), &[1, 2]), None);
        assert_eq!(decode(fourcc("sp78"), &[0x2A]), None, "short data is rejected");
    }

    #[test]
    fn labels_keys() {
        assert_eq!(temperature_label("TC0P"), ("CPU", "CPU proximity".to_string()));
        assert_eq!(temperature_label("TC2C"), ("CPU", "CPU core 2".to_string()));
        assert_eq!(temperature_label("Tp09").0, "CPU");
        assert_eq!(temperature_label("Tg0f").0, "GPU");
        assert_eq!(temperature_label("TB1T").0, "Battery");
        assert_eq!(temperature_label("Fxyz").0, "Other");
        assert_eq!(power_label("PSTR"), Some("System total"));
        assert_eq!(power_label("PXYZ"), None);
        assert!(plausible_temp(45.0) && !plausible_temp(0.0) && !plausible_temp(-127.0));
    }
}
