//! Background sampling thread. The UI never blocks on hardware queries: it
//! reads the latest snapshot and history from shared state.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::collect;
use crate::history::{History, derive_sensors};
use crate::model::{Snapshot, StaticInfo};

pub struct Shared {
    pub info: StaticInfo,
    pub latest: Option<Arc<Snapshot>>,
    pub history: History,
    pub samples: u64,
    pub last_cost: Duration,
    pub last_at: Option<SystemTime>,
    pub log_error: Option<String>,
}

pub struct Sampler {
    pub shared: Arc<RwLock<Shared>>,
    interval_ms: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    log_path: Arc<Mutex<Option<PathBuf>>>,
    handle: Option<JoinHandle<()>>,
}

impl Sampler {
    pub fn start(interval: Duration, window_secs: f64, repaint: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(RwLock::new(Shared {
            info: StaticInfo::default(),
            latest: None,
            history: History::new(window_secs),
            samples: 0,
            last_cost: Duration::ZERO,
            last_at: None,
            log_error: None,
        }));
        let interval_ms = Arc::new(AtomicU64::new(interval.as_millis() as u64));
        let paused = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let log_path: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

        let (sh, iv, pa, st, lp) =
            (shared.clone(), interval_ms.clone(), paused.clone(), stop.clone(), log_path.clone());
        let handle = std::thread::Builder::new()
            .name("opentemp-sampler".into())
            .spawn(move || {
                let mut collector = collect::platform();
                let info = collector.static_info();
                sh.write().unwrap().info = info;
                let mut logger: Option<CsvLogger> = None;
                while !st.load(Ordering::Relaxed) {
                    let tick = Instant::now();
                    if !pa.load(Ordering::Relaxed) {
                        let mut snap = collector.sample();
                        derive_sensors(&mut snap);
                        collect::trim_processes(&mut snap.processes, 300);
                        let cost = tick.elapsed();

                        // CSV logging, opened/closed as the setting changes.
                        let want = lp.lock().unwrap().clone();
                        let mut log_error = None;
                        match (&mut logger, want) {
                            (Some(l), Some(p)) if l.path == p => {
                                if let Err(e) = l.write(&snap) {
                                    log_error = Some(e.to_string());
                                }
                            }
                            (_, Some(p)) => match CsvLogger::open(p, &snap) {
                                Ok(mut l) => {
                                    if let Err(e) = l.write(&snap) {
                                        log_error = Some(e.to_string());
                                    }
                                    logger = Some(l);
                                }
                                Err(e) => {
                                    log_error = Some(e.to_string());
                                    logger = None;
                                }
                            },
                            (_, None) => logger = None,
                        }

                        {
                            let mut g = sh.write().unwrap();
                            g.history.record(&snap);
                            g.latest = Some(Arc::new(snap));
                            g.samples += 1;
                            g.last_cost = cost;
                            g.last_at = Some(SystemTime::now());
                            g.log_error = log_error;
                        }
                        repaint();
                    }
                    // Sleep in short slices so interval changes and shutdown are prompt.
                    let period = Duration::from_millis(iv.load(Ordering::Relaxed).max(100));
                    while tick.elapsed() < period && !st.load(Ordering::Relaxed) {
                        let left = period.saturating_sub(tick.elapsed());
                        std::thread::sleep(left.min(Duration::from_millis(50)));
                    }
                }
            })
            .expect("failed to start sampler thread");

        Self { shared, interval_ms, paused, stop, log_path, handle: Some(handle) }
    }

    pub fn set_interval(&self, d: Duration) {
        self.interval_ms.store(d.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_log_path(&self, p: Option<PathBuf>) {
        *self.log_path.lock().unwrap() = p;
    }

    pub fn set_window(&self, secs: f64) {
        self.shared.write().unwrap().history.window_secs = secs;
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Appends one row per sample. Columns are fixed when the file is opened.
struct CsvLogger {
    path: PathBuf,
    out: BufWriter<File>,
    gpu_keys: Vec<String>,
    sensor_ids: Vec<String>,
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

impl CsvLogger {
    fn open(path: PathBuf, first: &Snapshot) -> std::io::Result<Self> {
        let existed = path.exists() && std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut out = BufWriter::new(file);
        let sensor_ids: Vec<String> = first.sensors.iter().map(|s| s.id.clone()).collect();
        let gpu_keys: Vec<String> = first.gpus.iter().map(|g| g.key.clone()).collect();
        if !existed {
            let mut header = vec![
                "timestamp_unix".to_string(),
                "cpu_usage_pct".into(),
                "cpu_clock_mhz".into(),
                "cpu_temp_c".into(),
                "cpu_power_w".into(),
                "mem_used_bytes".into(),
                "mem_total_bytes".into(),
                "net_rx_bps".into(),
                "net_tx_bps".into(),
            ];
            for g in &first.gpus {
                header.push(format!("gpu_usage_pct[{}]", g.name));
            }
            for s in &first.sensors {
                header.push(format!("{} / {} ({})", s.group, s.label, s.kind.unit()));
            }
            writeln!(out, "{}", header.iter().map(|h| csv_escape(h)).collect::<Vec<_>>().join(","))?;
        }
        Ok(Self { path, out, gpu_keys, sensor_ids })
    }

    fn write(&mut self, s: &Snapshot) -> std::io::Result<()> {
        let opt = |v: Option<f64>| v.map(|v| format!("{v:.2}")).unwrap_or_default();
        let ts = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let mut row = vec![
            format!("{ts:.3}"),
            opt(s.cpu.usage.map(f64::from)),
            opt(s.cpu.avg_mhz),
            opt(s.cpu.temperature.map(f64::from)),
            opt(s.cpu.package_power_w.map(f64::from)),
            s.memory.used.to_string(),
            s.memory.total.to_string(),
            format!("{:.0}", s.networks.iter().map(|n| n.rx_bps).sum::<f64>()),
            format!("{:.0}", s.networks.iter().map(|n| n.tx_bps).sum::<f64>()),
        ];
        for key in &self.gpu_keys {
            row.push(opt(s.gpus.iter().find(|g| &g.key == key).and_then(|g| g.usage).map(f64::from)));
        }
        for id in &self.sensor_ids {
            row.push(opt(s.sensors.iter().find(|x| &x.id == id).map(|x| x.value)));
        }
        writeln!(self.out, "{}", row.join(","))?;
        self.out.flush()
    }
}
