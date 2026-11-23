//! NVIDIA GPUs through NVML, loaded at runtime so the app starts fine on
//! machines without an NVIDIA driver. Used on Windows and Linux.

#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_uint, c_void};

use libloading::Library;

use crate::model::{GpuSample, GpuVendor};

type Device = *mut c_void;
type Ret = c_int;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

struct Api {
    shutdown: unsafe extern "C" fn() -> Ret,
    count: unsafe extern "C" fn(*mut c_uint) -> Ret,
    handle: unsafe extern "C" fn(c_uint, *mut Device) -> Ret,
    name: unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Ret,
    uuid: unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Ret,
    util: unsafe extern "C" fn(Device, *mut Utilization) -> Ret,
    memory: unsafe extern "C" fn(Device, *mut Memory) -> Ret,
    temp: unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Ret,
    clock: unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Ret,
    max_clock: unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Ret,
    power: unsafe extern "C" fn(Device, *mut c_uint) -> Ret,
    power_limit: unsafe extern "C" fn(Device, *mut c_uint) -> Ret,
    fan: unsafe extern "C" fn(Device, *mut c_uint) -> Ret,
    encoder: unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> Ret,
    decoder: unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> Ret,
    driver: unsafe extern "C" fn(*mut c_char, c_uint) -> Ret,
}

pub struct Nvml {
    api: Api,
    devices: Vec<(Device, String, String)>,
    driver: Option<String>,
    // Keep the library loaded for as long as the function pointers live.
    _lib: Library,
}

// SAFETY: NVML is thread-safe; we only use it from the sampler thread.
unsafe impl Send for Nvml {}

const NVML_TEMPERATURE_GPU: c_int = 0;
const NVML_CLOCK_GRAPHICS: c_int = 0;
const NVML_CLOCK_MEM: c_int = 2;

fn candidates() -> Vec<String> {
    if cfg!(windows) {
        let mut v = vec!["nvml.dll".to_string()];
        if let Ok(pf) = std::env::var("ProgramW6432").or_else(|_| std::env::var("ProgramFiles")) {
            v.push(format!(r"{pf}\NVIDIA Corporation\NVSMI\nvml.dll"));
        }
        v
    } else {
        vec!["libnvidia-ml.so.1".into(), "libnvidia-ml.so".into()]
    }
}

fn cstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

impl Nvml {
    pub fn load() -> Option<Self> {
        if cfg!(target_os = "macos") {
            return None;
        }
        // SAFETY: loading a vendor library by its well-known name; NVML has no
        // library constructors with side effects beyond its own initialisation.
        let lib = candidates().into_iter().find_map(|p| unsafe { Library::new(p) }.ok())?;
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the signature matches the NVML header for this symbol.
                *unsafe { lib.get($name) }.ok()?
            };
        }
        let init: unsafe extern "C" fn() -> Ret = sym!(b"nvmlInit_v2\0");
        let api = Api {
            shutdown: sym!(b"nvmlShutdown\0"),
            count: sym!(b"nvmlDeviceGetCount_v2\0"),
            handle: sym!(b"nvmlDeviceGetHandleByIndex_v2\0"),
            name: sym!(b"nvmlDeviceGetName\0"),
            uuid: sym!(b"nvmlDeviceGetUUID\0"),
            util: sym!(b"nvmlDeviceGetUtilizationRates\0"),
            memory: sym!(b"nvmlDeviceGetMemoryInfo\0"),
            temp: sym!(b"nvmlDeviceGetTemperature\0"),
            clock: sym!(b"nvmlDeviceGetClockInfo\0"),
            max_clock: sym!(b"nvmlDeviceGetMaxClockInfo\0"),
            power: sym!(b"nvmlDeviceGetPowerUsage\0"),
            power_limit: sym!(b"nvmlDeviceGetEnforcedPowerLimit\0"),
            fan: sym!(b"nvmlDeviceGetFanSpeed\0"),
            encoder: sym!(b"nvmlDeviceGetEncoderUtilization\0"),
            decoder: sym!(b"nvmlDeviceGetDecoderUtilization\0"),
            driver: sym!(b"nvmlSystemGetDriverVersion\0"),
        };
        // SAFETY: documented entry points with matching signatures.
        unsafe {
            if init() != 0 {
                return None;
            }
            let mut n = 0;
            if (api.count)(&mut n) != 0 || n == 0 {
                (api.shutdown)();
                return None;
            }
            let mut devices = Vec::new();
            for i in 0..n {
                let mut d: Device = std::ptr::null_mut();
                if (api.handle)(i, &mut d) != 0 {
                    continue;
                }
                let mut buf = [0 as c_char; 96];
                let name = if (api.name)(d, buf.as_mut_ptr(), buf.len() as c_uint) == 0 {
                    cstr(&buf)
                } else {
                    format!("NVIDIA GPU {i}")
                };
                let mut ubuf = [0 as c_char; 96];
                let uuid = if (api.uuid)(d, ubuf.as_mut_ptr(), ubuf.len() as c_uint) == 0 {
                    cstr(&ubuf)
                } else {
                    format!("nvml{i}")
                };
                devices.push((d, name, uuid));
            }
            let mut dbuf = [0 as c_char; 96];
            let driver = ((api.driver)(dbuf.as_mut_ptr(), dbuf.len() as c_uint) == 0).then(|| cstr(&dbuf));
            Some(Self { api, devices, driver, _lib: lib })
        }
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        let a = &self.api;
        self.devices
            .iter()
            .map(|(d, name, uuid)| {
                let d = *d;
                let mut g = GpuSample {
                    key: uuid.clone(),
                    name: name.clone(),
                    vendor: GpuVendor::Nvidia,
                    driver: self.driver.clone(),
                    source: "NVML".into(),
                    ..Default::default()
                };
                // SAFETY: valid device handles from NVML; out-pointers are valid.
                unsafe {
                    let mut u = Utilization::default();
                    if (a.util)(d, &mut u) == 0 {
                        g.usage = Some(u.gpu.min(100) as f32);
                        g.engines.push(("3D / Compute".into(), u.gpu.min(100) as f32));
                    }
                    let (mut v, mut period) = (0, 0);
                    if (a.encoder)(d, &mut v, &mut period) == 0 {
                        g.engines.push(("Video Encode".into(), v.min(100) as f32));
                    }
                    if (a.decoder)(d, &mut v, &mut period) == 0 {
                        g.engines.push(("Video Decode".into(), v.min(100) as f32));
                    }
                    let mut m = Memory::default();
                    if (a.memory)(d, &mut m) == 0 {
                        g.dedicated_used = Some(m.used);
                        g.dedicated_total = Some(m.total);
                    }
                    let mut x = 0;
                    if (a.temp)(d, NVML_TEMPERATURE_GPU, &mut x) == 0 {
                        g.temperature = Some(x as f32);
                    }
                    if (a.clock)(d, NVML_CLOCK_GRAPHICS, &mut x) == 0 {
                        g.core_mhz = Some(x as f64);
                    }
                    if (a.max_clock)(d, NVML_CLOCK_GRAPHICS, &mut x) == 0 {
                        g.core_max_mhz = Some(x as f64);
                    }
                    if (a.clock)(d, NVML_CLOCK_MEM, &mut x) == 0 {
                        g.memory_mhz = Some(x as f64);
                    }
                    if (a.power)(d, &mut x) == 0 {
                        g.power_w = Some(x as f32 / 1000.0);
                    }
                    if (a.power_limit)(d, &mut x) == 0 {
                        g.power_limit_w = Some(x as f32 / 1000.0);
                    }
                    if (a.fan)(d, &mut x) == 0 {
                        g.fan_pct = Some(x as f32);
                    }
                }
                g
            })
            .collect()
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balanced with the successful nvmlInit_v2 in `load`.
        unsafe {
            (self.api.shutdown)();
        }
    }
}
