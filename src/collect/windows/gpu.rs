//! Vendor-neutral GPU monitoring on Windows.
//!
//! * DXGI enumerates adapters (name, vendor, memory sizes, driver version).
//! * PDH "GPU Engine" / "GPU Adapter Memory" counters give utilisation and
//!   memory use, the same source Task Manager uses. Works for any WDDM 2.x GPU.
//! * D3DKMT performance data adds temperature, fan, and clocks where the
//!   driver reports them (mostly discrete GPUs).

use std::collections::HashMap;

use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_CLOSEADAPTER, D3DKMT_NODE_PERFDATA, D3DKMT_OPENADAPTERFROMLUID,
    D3DKMT_QUERYADAPTERINFO, D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo,
    KMTQAITYPE_ADAPTERPERFDATA, KMTQAITYPE_NODEPERFDATA,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIDevice, IDXGIFactory1};
use windows::core::Interface;

use super::pdh::{Counter, Query};
use crate::model::{GpuSample, GpuVendor};

struct Adapter {
    luid_key: String,
    name: String,
    vendor: GpuVendor,
    integrated: bool,
    dedicated_total: u64,
    shared_total: u64,
    driver: Option<String>,
    kmt: Option<u32>,
}

pub struct Gpus {
    adapters: Vec<Adapter>,
    engine: Option<Counter>,
    dedicated: Option<Counter>,
    shared: Option<Counter>,
}

impl Gpus {
    pub fn new(query: &Query) -> Self {
        Self {
            adapters: enumerate(),
            engine: query.add(r"\GPU Engine(*)\Utilization Percentage"),
            dedicated: query.add(r"\GPU Adapter Memory(*)\Dedicated Usage"),
            shared: query.add(r"\GPU Adapter Memory(*)\Shared Usage"),
        }
    }

    pub fn has_counters(&self) -> bool {
        self.engine.is_some()
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        // (luid, engine index) -> (engine type, summed utilisation over processes)
        let mut engines: HashMap<(String, u32), (String, f64)> = HashMap::new();
        if let Some(c) = &self.engine {
            for (inst, v) in c.values() {
                let Some(p) = parse_engine_instance(&inst) else { continue };
                let e = engines.entry((p.luid, p.engine)).or_insert_with(|| (p.engtype, 0.0));
                e.1 += v;
            }
        }
        let mem = |c: &Option<Counter>| -> HashMap<String, u64> {
            let mut m = HashMap::new();
            if let Some(c) = c {
                for (inst, v) in c.values() {
                    if let Some(l) = luid_of(&inst) {
                        *m.entry(l).or_default() += v.max(0.0) as u64;
                    }
                }
            }
            m
        };
        let dedicated = mem(&self.dedicated);
        let shared = mem(&self.shared);

        let mut out = Vec::new();
        for a in &self.adapters {
            // Per engine type, take the busiest engine of that type.
            let mut by_type: HashMap<String, f64> = HashMap::new();
            for ((luid, _), (ty, v)) in &engines {
                if luid.eq_ignore_ascii_case(&a.luid_key) {
                    let e = by_type.entry(ty.clone()).or_default();
                    *e = e.max(*v);
                }
            }
            let mut list: Vec<(String, f32)> =
                by_type.into_iter().map(|(k, v)| (pretty_engine(&k), v.clamp(0.0, 100.0) as f32)).collect();
            list.sort_by(|a, b| engine_rank(&a.0).cmp(&engine_rank(&b.0)).then(a.0.cmp(&b.0)));
            let usage = list.iter().map(|e| e.1).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))));

            let mut g = GpuSample {
                key: a.luid_key.clone(),
                name: a.name.clone(),
                vendor: a.vendor,
                integrated: a.integrated,
                usage: if self.engine.is_some() { Some(usage.unwrap_or(0.0)) } else { None },
                engines: list,
                dedicated_used: dedicated.get(&a.luid_key.to_ascii_lowercase()).copied(),
                dedicated_total: (a.dedicated_total > 0).then_some(a.dedicated_total),
                shared_used: shared.get(&a.luid_key.to_ascii_lowercase()).copied(),
                shared_total: (a.shared_total > 0).then_some(a.shared_total),
                driver: a.driver.clone(),
                source: "PDH".into(),
                ..Default::default()
            };
            if let Some(h) = a.kmt {
                kmt_perf(h, &mut g);
            }
            out.push(g);
        }
        // DXGI can list one physical GPU under several LUIDs (e.g. a second
        // instance created for indirect displays). Only the LUID that carries
        // performance counters is real; hide the shadows.
        let has_data = |g: &GpuSample| g.dedicated_used.is_some() || !g.engines.is_empty();
        let shadows: Vec<bool> = out
            .iter()
            .map(|g| !has_data(g) && out.iter().any(|o| o.name == g.name && o.key != g.key && has_data(o)))
            .collect();
        let mut i = 0;
        out.retain(|_| {
            i += 1;
            !shadows[i - 1]
        });
        out
    }
}

impl Drop for Gpus {
    fn drop(&mut self) {
        for a in &self.adapters {
            if let Some(h) = a.kmt {
                let close = D3DKMT_CLOSEADAPTER { hAdapter: h };
                // SAFETY: closing a handle we opened.
                unsafe {
                    let _ = D3DKMTCloseAdapter(&close);
                }
            }
        }
    }
}

fn luid_key(l: LUID) -> String {
    format!("luid_0x{:08x}_0x{:08x}", l.HighPart as u32, l.LowPart)
}

fn enumerate() -> Vec<Adapter> {
    let mut out: Vec<Adapter> = Vec::new();
    // SAFETY: plain COM factory creation; the COM runtime is not required for DXGI.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    for i in 0.. {
        // SAFETY: EnumAdapters1 returns DXGI_ERROR_NOT_FOUND past the end.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        // SAFETY: adapter is a valid COM object.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let key = luid_key(desc.AdapterLuid);
        if out.iter().any(|a| a.luid_key == key) {
            continue;
        }
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len]).trim().to_string();
        let vendor = GpuVendor::from_pci_id(desc.VendorId);
        // SAFETY: valid adapter; returns the user-mode driver version.
        let driver = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.ok().map(|v| {
            let v = v as u64;
            format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xffff, (v >> 16) & 0xffff, v & 0xffff)
        });
        let mut open = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: desc.AdapterLuid, hAdapter: 0 };
        // SAFETY: valid in/out struct.
        let kmt = (unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_ok()).then_some(open.hAdapter);
        out.push(Adapter {
            luid_key: key,
            integrated: looks_integrated(vendor, &name, desc.DedicatedVideoMemory as u64),
            name,
            vendor,
            dedicated_total: desc.DedicatedVideoMemory as u64,
            shared_total: desc.SharedSystemMemory as u64,
            driver,
            kmt,
        });
    }
    out
}

/// DXGI has no "integrated" flag; infer it from the vendor and product name.
fn looks_integrated(vendor: GpuVendor, name: &str, dedicated: u64) -> bool {
    let n = name.to_ascii_lowercase();
    match vendor {
        GpuVendor::Intel => !n.contains("arc a") && !n.contains("arc b") && !n.contains("dg"),
        GpuVendor::Amd => {
            !(n.contains(" rx ") || n.contains("radeon pro") || n.contains("instinct") || n.contains("firepro"))
                && (n.contains("graphics") || n.contains("vega"))
        }
        GpuVendor::Apple => true,
        GpuVendor::Nvidia => false,
        _ => dedicated < 512 * 1024 * 1024,
    }
}

fn kmt_perf(handle: u32, g: &mut GpuSample) {
    let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: KMTQAITYPE_ADAPTERPERFDATA,
        pPrivateDriverData: (&mut perf as *mut D3DKMT_ADAPTER_PERFDATA).cast(),
        PrivateDriverDataSize: size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
    };
    // SAFETY: the private data pointer and size describe `perf`.
    if unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok() {
        if perf.Temperature > 0 {
            g.temperature = Some(perf.Temperature as f32 / 10.0);
        }
        if perf.FanRPM > 0 {
            g.fan_rpm = Some(perf.FanRPM);
        }
        if perf.MemoryFrequency > 0 {
            g.memory_mhz = Some(perf.MemoryFrequency as f64 / 1e6);
        }
        g.source = "PDH + D3DKMT".into();
    }
    let mut node = D3DKMT_NODE_PERFDATA { NodeOrdinal: 0, PhysicalAdapterIndex: 0, ..Default::default() };
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: KMTQAITYPE_NODEPERFDATA,
        pPrivateDriverData: (&mut node as *mut D3DKMT_NODE_PERFDATA).cast(),
        PrivateDriverDataSize: size_of::<D3DKMT_NODE_PERFDATA>() as u32,
    };
    // SAFETY: as above.
    if unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok() {
        if node.Frequency > 0 {
            g.core_mhz = Some(node.Frequency as f64 / 1e6);
        }
        if node.MaxFrequency > 0 {
            g.core_max_mhz = Some(node.MaxFrequency as f64 / 1e6);
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct EngineInstance {
    pub luid: String,
    pub engine: u32,
    pub engtype: String,
}

/// Lowercased `luid_0x..._0x...` key from a PDH GPU instance name.
pub(crate) fn luid_of(inst: &str) -> Option<String> {
    let lower = inst.to_ascii_lowercase();
    let start = lower.find("luid_0x")?;
    let rest = &lower[start..];
    // "luid_0x" + 8 hex + "_0x" + 8 hex = 26 chars
    (rest.len() >= 26).then(|| rest[..26].to_string())
}

/// Parse `pid_1234_luid_0x00000000_0x0000C6D3_phys_0_eng_3_engtype_VideoDecode`.
pub(crate) fn parse_engine_instance(inst: &str) -> Option<EngineInstance> {
    let luid = luid_of(inst)?;
    let eng_pos = inst.find("_eng_")?;
    let after = &inst[eng_pos + 5..];
    let (num, rest) = after.split_once('_')?;
    let engine = num.parse().ok()?;
    let engtype = rest.strip_prefix("engtype_")?.to_string();
    Some(EngineInstance { luid, engine, engtype })
}

fn pretty_engine(raw: &str) -> String {
    match raw {
        "3D" => "3D".into(),
        "VideoDecode" => "Video Decode".into(),
        "VideoEncode" => "Video Encode".into(),
        "VideoProcessing" => "Video Processing".into(),
        "LegacyOverlay" => "Overlay".into(),
        "" => "Other".into(),
        other if other.contains(' ') => other.to_string(),
        other => {
            // Split CamelCase ("SecurityEngine"), but keep acronyms ("JPEG") whole.
            let chars: Vec<char> = other.chars().collect();
            let mut s = String::new();
            for (i, &ch) in chars.iter().enumerate() {
                if i > 0 && ch.is_uppercase() && chars[i - 1].is_lowercase() {
                    s.push(' ');
                }
                s.push(ch);
            }
            s
        }
    }
}

fn engine_rank(name: &str) -> u8 {
    match name {
        "3D" => 0,
        "Compute" => 1,
        "Copy" => 2,
        "Video Decode" => 3,
        "Video Encode" => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_instance_names() {
        let p = parse_engine_instance("pid_1234_luid_0x00000000_0x0000C6D3_phys_0_eng_3_engtype_VideoDecode").unwrap();
        assert_eq!(p.luid, "luid_0x00000000_0x0000c6d3");
        assert_eq!(p.engine, 3);
        assert_eq!(p.engtype, "VideoDecode");
        assert!(parse_engine_instance("_Total").is_none());
        assert_eq!(luid_of("luid_0x00000000_0x0000D1B4_phys_0").as_deref(), Some("luid_0x00000000_0x0000d1b4"));
        assert_eq!(luid_key(LUID { LowPart: 0xd1b4, HighPart: 0 }), "luid_0x00000000_0x0000d1b4");
    }

    #[test]
    fn engine_names_are_readable() {
        assert_eq!(pretty_engine("VideoDecode"), "Video Decode");
        assert_eq!(pretty_engine("SecurityEngine"), "Security Engine");
        assert_eq!(pretty_engine("Video JPEG 0"), "Video JPEG 0");
        assert_eq!(pretty_engine("VideoJPEG"), "Video JPEG");
    }
}
