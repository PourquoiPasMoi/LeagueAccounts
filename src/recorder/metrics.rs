//! The recorder's own cost: CPU time of this process, its memory, and GPU use
//! from Windows' "GPU Engine" performance counters (the numbers Task Manager
//! shows). Read-only system counters, nothing about the game process.

use std::collections::HashMap;
use std::time::Instant;
use windows::core::{PCWSTR, HSTRING};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// Kernel + user CPU time of this process, in 100 ns units.
fn process_cpu() -> u64 {
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
    }
    ticks(kernel) + ticks(user)
}

pub struct CpuMeter {
    wall: Instant,
    cpu: u64,
}

impl CpuMeter {
    pub fn start() -> Self {
        Self { wall: Instant::now(), cpu: process_cpu() }
    }

    /// 100 = one logical core fully busy.
    pub fn percent_of_one_core(&self) -> f64 {
        let wall = self.wall.elapsed().as_secs_f64() * 10_000_000.0;
        if wall <= 0.0 {
            return 0.0;
        }
        process_cpu().saturating_sub(self.cpu) as f64 / wall * 100.0
    }

    /// Share of the whole processor (all logical cores), like Task Manager.
    pub fn percent_of_machine(&self) -> f64 {
        let cores = std::thread::available_parallelism().map(|cores| cores.get()).unwrap_or(1) as f64;
        self.percent_of_one_core() / cores
    }
}

/// This process's working set in MB.
pub fn memory_mb() -> f64 {
    let mut counters = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb);
    }
    counters.WorkingSetSize as f64 / 1_048_576.0
}

struct Counter {
    label: &'static str,
    handle: PDH_HCOUNTER,
    sum: f64,
    max: f64,
    samples: u32,
}

/// GPU engine use, sampled about once a second. Create it after the encoder
/// started: Windows lists a process's engines once it has used them.
pub struct GpuMeter {
    query: PDH_HQUERY,
    counters: Vec<Counter>,
}

impl GpuMeter {
    pub fn new() -> Option<Self> {
        let pid = unsafe { GetCurrentProcessId() };
        let mut query = PDH_HQUERY::default();
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }
        let paths: [(&'static str, String); 4] = [
            ("recorder video encode", format!(r"\GPU Engine(pid_{pid}_*engtype_VideoEncode)\Utilization Percentage")),
            ("recorder 3D (capture + convert)", format!(r"\GPU Engine(pid_{pid}_*engtype_3D)\Utilization Percentage")),
            ("whole PC video encode", r"\GPU Engine(*engtype_VideoEncode)\Utilization Percentage".to_owned()),
            ("whole PC 3D (game included)", r"\GPU Engine(*engtype_3D)\Utilization Percentage".to_owned()),
        ];
        let mut counters = Vec::new();
        for (label, path) in paths {
            let mut handle = PDH_HCOUNTER::default();
            if unsafe { PdhAddEnglishCounterW(query, &HSTRING::from(path), 0, &mut handle) } == 0 {
                counters.push(Counter { label, handle, sum: 0.0, max: 0.0, samples: 0 });
            }
        }
        unsafe { PdhCollectQueryData(query) };
        Some(Self { query, counters })
    }

    pub fn sample(&mut self) {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return;
        }
        for counter in &mut self.counters {
            if let Some(value) = busiest_engine(counter.handle) {
                counter.sum += value;
                counter.max = counter.max.max(value);
                counter.samples += 1;
            }
        }
    }

    /// (label, average %, peak %) per counter.
    pub fn results(&self) -> Vec<(&'static str, f64, f64)> {
        self.counters
            .iter()
            .map(|counter| (counter.label, if counter.samples > 0 { counter.sum / f64::from(counter.samples) } else { 0.0 }, counter.max))
            .collect()
    }
}

impl Drop for GpuMeter {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Use of the busiest matching engine (instances summed per engine, as Task Manager does).
fn busiest_engine(counter: PDH_HCOUNTER) -> Option<f64> {
    let (mut size, mut count) = (0u32, 0u32);
    let status = unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None) };
    if status != PDH_MORE_DATA {
        return (status == 0).then_some(0.0);
    }
    let mut buffer = vec![0u64; size as usize / 8 + 1];
    let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
    if unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) } != 0 {
        return None;
    }
    let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
    let mut engines: HashMap<String, f64> = HashMap::new();
    for item in items {
        if item.FmtValue.CStatus > 1 {
            continue;
        }
        let name = unsafe { item.szName.to_string() }.unwrap_or_default();
        let engine = name.find("luid").map(|start| name[start..].to_owned()).unwrap_or(name);
        *engines.entry(engine).or_default() += unsafe { item.FmtValue.Anonymous.doubleValue };
    }
    Some(engines.values().copied().fold(0.0, f64::max))
}
