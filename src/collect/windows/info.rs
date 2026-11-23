//! Static machine facts: OS version, CPU topology and caches.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationAll, RelationCache, RelationProcessorCore, RelationProcessorPackage,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::core::HSTRING;

use crate::model::{CpuInfo, StaticInfo};

pub fn reg_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let (k, v) = (HSTRING::from(key), HSTRING::from(value));
    let mut size = 0u32;
    // SAFETY: size query with no buffer.
    let r = unsafe { RegGetValueW(root, &k, &v, RRF_RT_REG_SZ, None, None, Some(&mut size)) };
    if r != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: buffer has `size` bytes.
    let r = unsafe { RegGetValueW(root, &k, &v, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]).trim().to_string())
}

pub fn reg_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let (k, v) = (HSTRING::from(key), HSTRING::from(value));
    let mut out = 0u32;
    let mut size = 4u32;
    // SAFETY: out-pointer to a u32 of the declared size.
    let r = unsafe {
        RegGetValueW(root, &k, &v, RRF_RT_REG_DWORD, None, Some((&mut out as *mut u32).cast()), Some(&mut size))
    };
    (r == ERROR_SUCCESS).then_some(out)
}

pub fn static_info() -> StaticInfo {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = reg_string(HKEY_LOCAL_MACHINE, NT, "CurrentBuild").and_then(|s| s.parse().ok()).unwrap_or(0);
    let ubr = reg_dword(HKEY_LOCAL_MACHINE, NT, "UBR").unwrap_or(0);
    let mut product = reg_string(HKEY_LOCAL_MACHINE, NT, "ProductName").unwrap_or_else(|| "Windows".into());
    // Windows 11 still reports "Windows 10" in ProductName; the build number tells them apart.
    if build >= 22000 {
        product = product.replace("Windows 10", "Windows 11");
    }
    let display = reg_string(HKEY_LOCAL_MACHINE, NT, "DisplayVersion").unwrap_or_default();

    let hostname = std::env::var("COMPUTERNAME").unwrap_or_default();

    StaticInfo {
        os_name: product,
        os_version: if display.is_empty() {
            format!("Build {build}.{ubr}")
        } else {
            format!("{display} (build {build}.{ubr})")
        },
        kernel: format!("NT {build}.{ubr}"),
        hostname,
        arch: std::env::consts::ARCH.to_string(),
        cpu: cpu_info(),
        memory_total: 0,
    }
}

fn cpu_info() -> CpuInfo {
    const CPU0: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    let (brand, vendor, features) = super::super::cpuid_info();
    let model = brand
        .or_else(|| reg_string(HKEY_LOCAL_MACHINE, CPU0, "ProcessorNameString"))
        .unwrap_or_else(|| "Unknown processor".into());
    let vendor = vendor.or_else(|| reg_string(HKEY_LOCAL_MACHINE, CPU0, "VendorIdentifier")).unwrap_or_default();
    let base_mhz = reg_dword(HKEY_LOCAL_MACHINE, CPU0, "~MHz").map(f64::from);

    let mut info = CpuInfo { model, vendor, base_mhz, features, ..Default::default() };
    topology(&mut info);
    if info.logical_cores == 0 {
        info.logical_cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        info.physical_cores = info.logical_cores;
    }
    info
}

/// Walk GetLogicalProcessorInformationEx for cores, packages, caches and
/// per-core efficiency classes (hybrid CPUs).
fn topology(info: &mut CpuInfo) {
    let mut len = 0u32;
    // SAFETY: size query.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &mut len) };
    if len == 0 {
        return;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of at least `len` bytes, 8-byte aligned.
    if unsafe { GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr().cast()), &mut len) }.is_err() {
        return;
    }
    let base = buf.as_ptr() as *const u8;
    let mut offset = 0usize;
    let mut classes: Vec<(usize, u8)> = Vec::new();
    let (mut l1, mut l2, mut l3) = (0u64, 0u64, 0u64);
    while offset < len as usize {
        // SAFETY: each record starts inside the buffer and declares its own size.
        let rec = unsafe { &*(base.add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX) };
        if rec.Size == 0 {
            break;
        }
        match rec.Relationship {
            r if r == RelationProcessorCore => {
                info.physical_cores += 1;
                // SAFETY: Processor is the active member for this relationship.
                let p = unsafe { &rec.Anonymous.Processor };
                let masks = unsafe { std::slice::from_raw_parts(p.GroupMask.as_ptr(), p.GroupCount.max(1) as usize) };
                for g in masks {
                    for bit in 0..usize::BITS as usize {
                        if g.Mask & (1usize << bit) != 0 {
                            classes.push((g.Group as usize * 64 + bit, p.EfficiencyClass));
                        }
                    }
                }
            }
            r if r == RelationProcessorPackage => info.sockets += 1,
            r if r == RelationCache => {
                // SAFETY: Cache is the active member for this relationship.
                let c = unsafe { &rec.Anonymous.Cache };
                let size = u64::from(c.CacheSize);
                match c.Level {
                    1 => l1 += size,
                    2 => l2 += size,
                    3 => l3 += size,
                    _ => {}
                }
            }
            _ => {}
        }
        offset += rec.Size as usize;
    }
    info.logical_cores = classes.len();
    info.l1_bytes = (l1 > 0).then_some(l1);
    info.l2_bytes = (l2 > 0).then_some(l2);
    info.l3_bytes = (l3 > 0).then_some(l3);
    classes.sort_by_key(|c| c.0);
    let distinct = classes.iter().map(|c| c.1).collect::<std::collections::BTreeSet<_>>().len();
    info.core_classes =
        if distinct > 1 { classes.iter().map(|c| Some(c.1)).collect() } else { vec![None; classes.len()] };
}
