//! Process list from a single NtQuerySystemInformation call (cheap: no
//! per-process handles are opened).

use std::time::Instant;

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

use crate::collect::{ProcessCpuTracker, RateTracker};
use crate::model::ProcessSample;

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

#[derive(Default)]
pub struct Processes {
    cpu: ProcessCpuTracker,
    io: RateTracker<(u32, u64, bool)>,
    buf: Vec<u64>,
}

pub struct ProcessTotals {
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
}

impl Processes {
    pub fn sample(&mut self, logical_cpus: usize, now: Instant) -> (Vec<ProcessSample>, Option<ProcessTotals>) {
        if self.buf.is_empty() {
            self.buf = vec![0u64; 512 * 1024 / 8];
        }
        let mut status = 0i32;
        for _ in 0..5 {
            let mut needed = 0u32;
            // SAFETY: the buffer is valid for its byte length.
            status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    self.buf.as_mut_ptr().cast(),
                    (self.buf.len() * 8) as u32,
                    &mut needed,
                )
            }
            .0;
            if status == STATUS_INFO_LENGTH_MISMATCH {
                // Leave headroom: processes may start between calls.
                self.buf = vec![0u64; (needed as usize + 64 * 1024).div_ceil(8)];
                continue;
            }
            break;
        }
        if status < 0 {
            return (Vec::new(), None);
        }

        self.cpu.begin(now);
        let mut out = Vec::with_capacity(400);
        let mut totals = ProcessTotals { processes: 0, threads: 0, handles: 0 };
        let base = self.buf.as_ptr() as *const u8;
        let mut offset = 0usize;
        loop {
            // SAFETY: entries are laid out back to back inside the buffer.
            let p = unsafe { &*(base.add(offset) as *const SYSTEM_PROCESS_INFORMATION) };
            let pid = p.UniqueProcessId.0 as usize as u32;
            totals.threads += p.NumberOfThreads;
            totals.handles += p.HandleCount;
            if pid != 0 {
                totals.processes += 1;
                // Reserved1 holds WorkingSetPrivateSize, HardFaultCount, ThreadsHighWatermark,
                // CycleTime, CreateTime, UserTime, KernelTime (documented in winternl's full form).
                let r = &p.Reserved1;
                let field = |at: usize| i64::from_le_bytes(r[at..at + 8].try_into().unwrap());
                let private_ws = field(0).max(0) as u64;
                let create = field(24) as u64;
                let cpu_100ns = field(32) + field(40);
                let name = if p.ImageName.Buffer.is_null() {
                    String::new()
                } else {
                    // SAFETY: Length is in bytes and the buffer lives inside our allocation.
                    let s =
                        unsafe { std::slice::from_raw_parts(p.ImageName.Buffer.0, p.ImageName.Length as usize / 2) };
                    String::from_utf16_lossy(s)
                };
                let cpu_pct = self.cpu.percent(pid, create, cpu_100ns as f64 / 1e7, logical_cpus);
                let read = self.io.rate(&(pid, create, false), p.Reserved7[3] as u64, now);
                let write = self.io.rate(&(pid, create, true), p.Reserved7[4] as u64, now);
                out.push(ProcessSample {
                    pid,
                    name: if name.is_empty() { format!("PID {pid}") } else { name },
                    cpu_pct,
                    memory: private_ws,
                    threads: p.NumberOfThreads,
                    read_bps: read,
                    write_bps: write,
                });
            }
            if p.NextEntryOffset == 0 {
                break;
            }
            offset += p.NextEntryOffset as usize;
        }
        self.cpu.end();
        let alive: std::collections::HashSet<u32> = out.iter().map(|p| p.pid).collect();
        self.io.retain(|k| alive.contains(&k.0));
        (out, Some(totals))
    }
}
