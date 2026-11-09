//! Background jobs (indexing, searching) that keep the UI responsive.

use nerka_core::Progress;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

pub struct Job<T> {
    pub progress: Progress,
    pub started: Instant,
    rx: Receiver<nerka_core::Result<T>>,
}

pub enum Poll<T> {
    Running,
    Done(nerka_core::Result<T>, Duration),
}

impl<T: Send + 'static> Job<T> {
    /// Run `work` on a thread of its own. A panic in it is an error saying
    /// so, not a cancel (review 2026-11-09, R8: a cancel is ignored, so the
    /// tab stayed on "loading" or kept its old result without a word).
    pub fn spawn(
        ctx: &eframe::egui::Context,
        work: impl FnOnce(&Progress) -> nerka_core::Result<T> + Send + 'static,
    ) -> Self {
        let progress = Progress::new();
        let (tx, rx) = channel();
        let (p, ctx) = (progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&p))).unwrap_or_else(|panic| {
                Err(nerka_core::Error::Format(format!("internal error: {}", message(&*panic))))
            });
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        Self { progress, started: Instant::now(), rx }
    }

    pub fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(r) => Poll::Done(r, self.started.elapsed()),
            Err(TryRecvError::Empty) => Poll::Running,
            Err(TryRecvError::Disconnected) => Poll::Done(Err(nerka_core::Error::Cancelled), self.started.elapsed()),
        }
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }
}

/// A panic's message, as `panic!` was given it.
fn message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a background task panicked")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait<T: Send + 'static>(job: &Job<T>) -> nerka_core::Result<T> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Poll::Done(r, _) = job.poll() {
                return r;
            }
            assert!(Instant::now() < until, "the job never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_job_that_panics_is_an_error_not_a_cancel() {
        let ctx = eframe::egui::Context::default();
        let job: Job<u32> = Job::spawn(&ctx, |_| panic!("index out of range"));
        match wait(&job) {
            Err(nerka_core::Error::Format(m)) => assert_eq!(m, "internal error: index out of range"),
            other => panic!("expected an internal error, got {other:?}"),
        }
        let job: Job<u32> = Job::spawn(&ctx, |_| panic!("{} lines", 3));
        assert!(matches!(wait(&job), Err(nerka_core::Error::Format(m)) if m == "internal error: 3 lines"));
        let job = Job::spawn(&ctx, |_| Ok(7));
        assert!(matches!(wait(&job), Ok(7)));
    }
}
