//! Volumes (capacity + throughput) and physical drive temperatures.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW,
    GetLogicalDriveStringsW, GetVolumeInformationW, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
    STORAGE_TEMPERATURE_DATA_DESCRIPTOR, StorageDeviceProperty, StorageDeviceTemperatureProperty,
};
use windows::core::HSTRING;

use super::pdh::{Counter, Query};
use crate::collect::nvme;
use crate::model::{DiskSample, DriveHealth, Sensor, SensorKind};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_RAMDISK: u32 = 6;

pub struct Disks {
    read: Option<Counter>,
    write: Option<Counter>,
    idle: Option<Counter>,
    drives: Vec<PhysicalDrive>,
}

struct PhysicalDrive {
    index: u32,
    model: String,
    handle: HANDLE,
}

// SAFETY: the handles are only used from the sampler thread.
unsafe impl Send for PhysicalDrive {}

impl Drop for PhysicalDrive {
    fn drop(&mut self) {
        // SAFETY: closing a handle we opened.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Disks {
    pub fn new(query: &Query) -> Self {
        Self {
            read: query.add(r"\LogicalDisk(*)\Disk Read Bytes/sec"),
            write: query.add(r"\LogicalDisk(*)\Disk Write Bytes/sec"),
            idle: query.add(r"\LogicalDisk(*)\% Idle Time"),
            drives: open_physical_drives(),
        }
    }

    pub fn sample(&self) -> Vec<DiskSample> {
        let lookup = |c: &Option<Counter>| -> Vec<(String, f64)> { c.as_ref().map(|c| c.values()).unwrap_or_default() };
        let (reads, writes, idles) = (lookup(&self.read), lookup(&self.write), lookup(&self.idle));
        let find = |list: &[(String, f64)], inst: &str| {
            list.iter().find(|(n, _)| n.eq_ignore_ascii_case(inst)).map(|(_, v)| *v)
        };

        let mut buf = [0u16; 512];
        // SAFETY: buffer is large enough for 26 "X:\\\0" entries.
        let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
        let mut out = Vec::new();
        for root in buf[..n.min(buf.len())].split(|&c| c == 0).filter(|s| !s.is_empty()) {
            let root_str = String::from_utf16_lossy(root);
            let root_h = HSTRING::from(root_str.as_str());
            // SAFETY: valid root path.
            let ty = unsafe { GetDriveTypeW(&root_h) };
            if !matches!(ty, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_REMOTE | DRIVE_RAMDISK) {
                continue;
            }
            let (mut total, mut free) = (0u64, 0u64);
            // SAFETY: valid out-pointers. Fails for empty card readers, which we skip.
            if unsafe { GetDiskFreeSpaceExW(&root_h, None, Some(&mut total), Some(&mut free)) }.is_err() || total == 0 {
                continue;
            }
            let mut label = [0u16; 261];
            let mut fs = [0u16; 261];
            // SAFETY: buffers sized as documented (MAX_PATH + 1).
            let _ = unsafe { GetVolumeInformationW(&root_h, Some(&mut label), None, None, None, Some(&mut fs)) };
            let w = |s: &[u16]| {
                let l = s.iter().position(|&c| c == 0).unwrap_or(s.len());
                String::from_utf16_lossy(&s[..l])
            };
            let inst = root_str.trim_end_matches('\\').to_string(); // "C:"
            let label = w(&label);
            let busy = find(&idles, &inst).map(|idle| (100.0 - idle).clamp(0.0, 100.0) as f32);
            out.push(DiskSample {
                key: inst.clone(),
                name: if label.is_empty() { format!("Local Disk ({inst})") } else { format!("{label} ({inst})") },
                mount: root_str.clone(),
                fs: w(&fs),
                total,
                free,
                removable: ty == DRIVE_REMOVABLE,
                read_bps: find(&reads, &inst),
                write_bps: find(&writes, &inst),
                busy_pct: busy,
            });
        }
        out
    }

    /// Drive temperatures and health. NVMe drives answer the SMART / health
    /// log query; other drives fall back to the generic temperature property.
    pub fn health(&self) -> (Vec<Sensor>, Vec<DriveHealth>) {
        let mut sensors = Vec::new();
        let mut drives = Vec::new();
        for d in &self.drives {
            let group = format!("Drive {} · {}", d.index, d.model);
            let sensor = |i: usize, label: String, value: f64, high: Option<f64>, critical: Option<f64>| Sensor {
                id: format!("drive{}.{}", d.index, i),
                group: group.clone(),
                label,
                kind: SensorKind::Temperature,
                value,
                high,
                critical,
            };
            let mut h =
                DriveHealth { key: format!("PhysicalDrive{}", d.index), model: d.model.clone(), ..Default::default() };
            if let Some(n) = nvme_health_log(d.handle).and_then(|log| nvme::parse_health_log(&log)) {
                h.temperature = n.composite_c;
                h.life_used_pct = Some(n.life_used_pct);
                h.spare_pct = Some(n.spare_pct);
                h.power_on_hours = Some(n.power_on_hours);
                h.bytes_read = Some(n.bytes_read);
                h.bytes_written = Some(n.bytes_written);
                h.unsafe_shutdowns = Some(n.unsafe_shutdowns);
                h.media_errors = Some(n.media_errors);
                h.critical_warning = Some(n.critical_warning);
                if let Some(c) = n.composite_c {
                    sensors.push(sensor(0, "Composite".into(), c as f64, None, None));
                }
                for (i, c) in n.sensors_c {
                    sensors.push(sensor(i + 1, format!("Sensor {}", i + 1), c as f64, None, None));
                }
            } else if let Some((temps, warn, crit)) = temperature_property(d.handle) {
                h.temperature = temps.first().map(|t| t.1 as f32);
                for (i, t) in temps {
                    let label = if i == 0 { "Composite".to_string() } else { format!("Sensor {i}") };
                    sensors.push(sensor(i, label, t, warn, crit));
                }
            }
            drives.push(h);
        }
        (sensors, drives)
    }
}

fn query_property(handle: HANDLE, buf: &mut [u32], in_len: usize) -> Option<usize> {
    let mut got = 0u32;
    let p = buf.as_mut_ptr();
    // SAFETY: the same buffer serves as input (first `in_len` bytes) and output.
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(p as *const _),
            in_len as u32,
            Some(p.cast()),
            (buf.len() * 4) as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    Some(got as usize)
}

/// Ask the NVMe driver for log page 0x02 through IOCTL_STORAGE_QUERY_PROPERTY.
/// Input: STORAGE_PROPERTY_QUERY header (8 bytes) + STORAGE_PROTOCOL_SPECIFIC_DATA (40 bytes),
/// output: descriptor header (8 bytes) + the same 40 bytes + 512 bytes of log.
fn nvme_health_log(handle: HANDLE) -> Option<Vec<u8>> {
    const STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY: u32 = 50;
    const PROTOCOL_TYPE_NVME: u32 = 3;
    const NVME_DATA_TYPE_LOG_PAGE: u32 = 2;
    const NVME_LOG_HEALTH: u32 = 2;
    const LOG_LEN: usize = 512;
    let mut buf = vec![0u32; (8 + 40 + LOG_LEN) / 4];
    buf[0] = STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY;
    buf[1] = PropertyStandardQuery.0 as u32;
    buf[2] = PROTOCOL_TYPE_NVME;
    buf[3] = NVME_DATA_TYPE_LOG_PAGE;
    buf[4] = NVME_LOG_HEALTH;
    buf[6] = 40; // ProtocolDataOffset, relative to the protocol struct
    buf[7] = LOG_LEN as u32;
    let len = buf.len() * 4;
    let got = query_property(handle, &mut buf, len)?;
    if got < 48 + 216 {
        return None;
    }
    let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
    let offset = 8 + u32::from_le_bytes(bytes[24..28].try_into().ok()?) as usize;
    bytes.get(offset..offset + LOG_LEN).map(|s| s.to_vec())
}

type TempProperty = (Vec<(usize, f64)>, Option<f64>, Option<f64>);

fn temperature_property(handle: HANDLE) -> Option<TempProperty> {
    let mut buf = vec![0u32; 128];
    buf[0] = StorageDeviceTemperatureProperty.0 as u32;
    buf[1] = PropertyStandardQuery.0 as u32;
    let got = query_property(handle, &mut buf, size_of::<STORAGE_PROPERTY_QUERY>())?;
    if got < size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>() {
        return None;
    }
    // SAFETY: the driver filled at least one descriptor; the buffer is 4-byte aligned like the struct.
    let desc = unsafe { &*(buf.as_ptr() as *const STORAGE_TEMPERATURE_DATA_DESCRIPTOR) };
    let count = (desc.InfoCount as usize).clamp(1, 8);
    // SAFETY: the buffer (512 bytes) holds at least 8 STORAGE_TEMPERATURE_INFO records after the header.
    let infos = unsafe { std::slice::from_raw_parts(desc.TemperatureInfo.as_ptr(), count) };
    let temps = infos
        .iter()
        .enumerate()
        .filter(|(_, i)| i.Temperature > 0 && i.Temperature <= 150)
        .map(|(n, i)| (n, i.Temperature as f64))
        .collect();
    let warn = (desc.WarningTemperature > 0).then_some(desc.WarningTemperature as f64);
    let crit = (desc.CriticalTemperature > 0).then_some(desc.CriticalTemperature as f64);
    Some((temps, warn, crit))
}

/// Open `\\.\PhysicalDriveN` with no access rights: enough for property
/// queries, and it does not need administrator rights.
fn open_physical_drives() -> Vec<PhysicalDrive> {
    let mut out = Vec::new();
    for index in 0..32u32 {
        let path = HSTRING::from(format!(r"\\.\PhysicalDrive{index}"));
        // SAFETY: plain CreateFileW with zero desired access.
        let Ok(handle) = (unsafe {
            CreateFileW(
                &path,
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }) else {
            continue;
        };
        let model = device_model(handle).unwrap_or_else(|| format!("PhysicalDrive{index}"));
        out.push(PhysicalDrive { index, model, handle });
    }
    out
}

fn device_model(handle: HANDLE) -> Option<String> {
    let mut query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        ..Default::default()
    };
    let mut buf = [0u64; 128];
    let mut got = 0u32;
    // SAFETY: buffers sized correctly.
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&mut query as *mut STORAGE_PROPERTY_QUERY).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr().cast()),
            size_of_val(&buf) as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, (got as usize).min(size_of_val(&buf))) };
    // SAFETY: the buffer starts with the descriptor.
    let desc = unsafe { &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
    let read_str = |off: u32| -> Option<String> {
        let off = off as usize;
        if off == 0 || off >= bytes.len() {
            return None;
        }
        let end = bytes[off..].iter().position(|&b| b == 0).map_or(bytes.len(), |p| off + p);
        let s = String::from_utf8_lossy(&bytes[off..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    let vendor = read_str(desc.VendorIdOffset);
    let product = read_str(desc.ProductIdOffset)?;
    Some(match vendor {
        Some(v) if !product.starts_with(&v) => format!("{v} {product}"),
        _ => product,
    })
}
