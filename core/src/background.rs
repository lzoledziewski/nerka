//! Work nobody waits for (a tab's signals, its checks, the plot's lanes)
//! gives way to work someone waits for (an open, a filter, a search).
//!
//! Owner, 2026-11-23: filtering takes precedence over background work,
//! above all when reading the file is slow (a file over NFS). Before, both
//! ran on the one thread pool and shared the CPU and the reads: a filter
//! typed after an open waited for a tab's signals and lifecycles.
//!
//! Background work runs on a pool of its own ([`run`]); while any
//! [`Foreground`] is held, its threads wait at their next per-chunk check
//! ([`crate::Progress::is_cancelled`]), so the waited-for work has every
//! core and the disk to itself. Only the background pool's threads wait:
//! work on the global pool (and the UI thread polling a job) never does,
//! so the waited-for work can't end up waiting for the background. The
//! pool's threads also run at the lowest priority (`SCHED_IDLE` on Linux,
//! background mode on Windows): what doesn't stop at a check still leaves
//! the CPU to the UI and the filter.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// Foreground work running now.
static RUNNING: AtomicUsize = AtomicUsize::new(0);
/// Taken to change [`RUNNING`] and to wait for it to reach 0.
static LOCK: Mutex<()> = Mutex::new(());
static FREE: Condvar = Condvar::new();
static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

/// Held while work someone waits for runs: background work waits.
#[must_use = "background work waits only while this is held"]
pub struct Foreground(());

/// Mark the work running from now until the guard drops as waited for.
pub fn foreground() -> Foreground {
    let _l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    RUNNING.fetch_add(1, Ordering::SeqCst);
    Foreground(())
}

impl Drop for Foreground {
    fn drop(&mut self) {
        let _l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if RUNNING.fetch_sub(1, Ordering::SeqCst) == 1 {
            FREE.notify_all();
        }
    }
}

/// Whether work someone waits for is running.
pub fn foreground_running() -> bool {
    RUNNING.load(Ordering::SeqCst) > 0
}

/// Start the background pool's threads now. An application calls this
/// at startup: started on first use instead, its threads were created
/// while a filter ran, and that filter sometimes took 1–2 s instead of
/// 0.1 s (measured in the Linux VM, 1 GiB DLT, 2026-11-23).
pub fn start() {
    pool();
}

fn pool() -> &'static rayon::ThreadPool {
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("nerka-background-{i}"))
            .start_handler(|_| lower_priority())
            .build()
            .expect("a thread pool for background work")
    })
}

/// Run the calling thread only when nothing else wants the CPU, so the
/// UI thread and the filter never wait for background work's parts that
/// don't stop at a check (a signal's values, a lifecycle's pass, the
/// plot's lanes). Measured (Linux VM, 1 GiB DLT, 4 signals, a lifecycle,
/// a check, 3 plotted): without, a frame took up to 0.9 s while they ran.
/// Best effort: where it can't be done, background work still waits at
/// its checks.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    {
        let param = libc::sched_param { sched_priority: 0 };
        // SAFETY: changes only the calling thread's policy (pid 0); `param`
        // is valid for the call.
        unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) };
    }
    #[cfg(windows)]
    {
        // Lowest CPU, I/O and memory priority for this thread.
        const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;
        unsafe extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(thread: isize, priority: i32) -> i32;
        }
        // SAFETY: the pseudo handle of the calling thread, which may set
        // its own priority.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }
}

/// Run `work` as background work: on the background pool (its parallel
/// parts too), waiting at each per-chunk check while foreground work runs.
pub fn run<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    pool().install(work)
}

/// On a background thread, wait while foreground work runs (or until
/// `cancelled`). Anywhere else, return at once.
pub(crate) fn give_way(cancelled: &AtomicBool) {
    if RUNNING.load(Ordering::Relaxed) == 0 || POOL.get().is_none_or(|p| p.current_thread_index().is_none()) {
        return;
    }
    let mut l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    while RUNNING.load(Ordering::SeqCst) > 0 && !cancelled.load(Ordering::Relaxed) {
        // A cancel doesn't notify: seen within this.
        l = FREE.wait_timeout(l, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Progress;
    use rayon::prelude::*;
    use std::sync::Arc;
    use std::time::Instant;

    /// Background work stops at its checks while a foreground guard is
    /// held and goes on when it drops; work elsewhere never waits; a
    /// cancel ends the wait. (One test: the guards are global.)
    #[test]
    fn background_work_waits_for_the_foreground() {
        let progress = Progress::new();
        let done = Arc::new(AtomicUsize::new(0));
        let guard = foreground();
        // Not on the background pool: never waits.
        assert!(!progress.is_cancelled());
        let (p, d) = (progress.clone(), done.clone());
        let worker = std::thread::spawn(move || {
            run(|| {
                (0..64).into_par_iter().for_each(|_| {
                    if !p.is_cancelled() {
                        d.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
        });
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(done.load(Ordering::SeqCst), 0, "ran while the foreground did");
        let t = Instant::now();
        drop(guard);
        worker.join().unwrap();
        assert_eq!(done.load(Ordering::SeqCst), 64);
        assert!(t.elapsed() < Duration::from_secs(5));

        let progress = Progress::new();
        let _guard = foreground();
        let p = progress.clone();
        let worker = std::thread::spawn(move || run(|| p.is_cancelled()));
        std::thread::sleep(Duration::from_millis(100));
        progress.cancel();
        assert!(worker.join().unwrap(), "the check says it was cancelled");
    }
}
