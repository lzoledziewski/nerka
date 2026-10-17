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
    pub fn spawn(
        ctx: &eframe::egui::Context,
        work: impl FnOnce(&Progress) -> nerka_core::Result<T> + Send + 'static,
    ) -> Self {
        let progress = Progress::new();
        let (tx, rx) = channel();
        let (p, ctx) = (progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work(&p));
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
