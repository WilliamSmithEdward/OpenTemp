//! A small safe wrapper over the Performance Data Helper (PDH) API.
//!
//! Counters are added by their English path so the app works on every display
//! language. Wildcard instances (`(*)`) are expanded on every collection, so
//! processes and devices that appear later are picked up automatically.

use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::core::{HSTRING, PCWSTR};

/// Do not clamp percentages at 100 (some counters, like `% Processor Performance`, exceed it).
const PDH_FMT_NOCAP100: u32 = 0x8000;

pub struct Query {
    handle: PDH_HQUERY,
}

// SAFETY: a PDH query handle can be used from any thread; we only use it from
// the sampler thread that owns the collector.
unsafe impl Send for Query {}

#[derive(Clone, Copy)]
pub struct Counter(PDH_HCOUNTER);

// SAFETY: see `Query`.
unsafe impl Send for Counter {}

impl Query {
    pub fn new() -> Option<Self> {
        let mut handle = PDH_HQUERY::default();
        // SAFETY: valid out-pointer; a null data source means "live data".
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        (status == 0).then_some(Self { handle })
    }

    /// Add a counter by its English path. Returns `None` if this machine does
    /// not have that counter (for example, no GPU counters in a VM).
    pub fn add(&self, path: &str) -> Option<Counter> {
        let mut counter = PDH_HCOUNTER::default();
        let wide = HSTRING::from(path);
        // SAFETY: the query handle is open and the path is a valid wide string.
        let status = unsafe { PdhAddEnglishCounterW(self.handle, &wide, 0, &mut counter) };
        (status == 0).then_some(Counter(counter))
    }

    pub fn collect(&self) -> bool {
        // SAFETY: the query handle is open.
        unsafe { PdhCollectQueryData(self.handle) == 0 }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: closing the handle we opened; counters die with it.
        unsafe {
            PdhCloseQuery(self.handle);
        }
    }
}

fn valid(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

impl Counter {
    /// The value of a single-instance counter.
    pub fn value(&self) -> Option<f64> {
        let mut v = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: valid counter handle and out-pointer.
        let status =
            unsafe { PdhGetFormattedCounterValue(self.0, PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100), None, &mut v) };
        // SAFETY: we asked for PDH_FMT_DOUBLE, so the double member is the active one.
        (status == 0 && valid(v.CStatus)).then_some(unsafe { v.Anonymous.doubleValue })
    }

    /// All instances of a wildcard counter as `(instance name, value)`.
    /// Instances whose value is not ready yet (new since the last collection) are skipped.
    pub fn values(&self) -> Vec<(String, f64)> {
        let fmt = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
        let mut size = 0u32;
        let mut count = 0u32;
        // SAFETY: first call with no buffer asks for the required size.
        let status = unsafe { PdhGetFormattedCounterArrayW(self.0, fmt, &mut size, &mut count, None) };
        if status != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // The buffer holds the item array followed by the instance name strings;
        // allocate it as u64 so the items are correctly aligned.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        // SAFETY: the buffer is at least `size` bytes and 8-byte aligned.
        let status = unsafe { PdhGetFormattedCounterArrayW(self.0, fmt, &mut size, &mut count, Some(items)) };
        if status != 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: PDH wrote `count` items into the buffer.
            let item = unsafe { &*items.add(i) };
            if !valid(item.FmtValue.CStatus) {
                continue;
            }
            // SAFETY: the name points into the same buffer and is NUL-terminated.
            let name = unsafe { item.szName.to_string() }.unwrap_or_default();
            // SAFETY: we asked for PDH_FMT_DOUBLE.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            out.push((name, value));
        }
        out
    }
}
