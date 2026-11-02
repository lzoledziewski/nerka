//! Memory the app holds itself (its heap: indexes, results, caches), not
//! the files it maps: those pages belong to the files and come and go.

/// Bytes of memory private to the process, if the system says.
#[cfg(windows)]
pub fn private_bytes() -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters { cb: size_of::<ProcessMemoryCounters>() as u32, ..Default::default() };
    // SAFETY: the current process's pseudo handle, and a counters struct of
    // the size given, which the call only writes into.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    // The commit charge: memory the process wrote, not mapped file pages.
    (ok != 0).then_some(c.pagefile_usage as u64)
}

/// Bytes of memory private to the process, if the system says: resident
/// pages that aren't shared (a mapped file's are).
#[cfg(target_os = "linux")]
pub fn private_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut n = statm.split_whitespace().skip(1).map(|v| v.parse::<u64>().ok());
    let (resident, shared) = (n.next()??, n.next()??);
    Some(resident.saturating_sub(shared) * 4096)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn private_bytes() -> Option<u64> {
    None
}
