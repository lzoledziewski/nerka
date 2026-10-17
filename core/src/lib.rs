//! Nerka core engine.
//!
//! The engine is UI-agnostic: the CLI, the desktop UI and (later) a server
//! all sit on top of the same building blocks (layout follows
//! `doc/nerka_architecture_implementation_spec_v_0.md` §10):
//!
//! * [`input`] — [`LogSource`]: random access to lines whatever the on-disk
//!   format (plain text today; PCAP, DLT, stdin later).
//! * [`engine`] — [`LineIndex`] (parallel line indexing) and [`search`]:
//!   filtering that yields the set of matching lines.
//! * [`dsl`] — NFD, the filter language (`level >= ERROR AND raw ~ /SSL/`).
//! * [`event`] — lazily normalized fields (level, …) per line.
//! * [`bookmarks`] — bookmarks/notes persisted in a sidecar file.

pub mod bookmarks;
pub mod dsl;
pub mod engine;
pub mod event;
pub mod input;

pub use bookmarks::Bookmarks;
pub use dsl::{DslError, Query};
pub use engine::index::LineIndex;
pub use engine::search::{Filter, Highlighter, Matcher, MatcherOptions, SearchResult, search};
pub use event::Level;
pub use input::LogSource;
pub use input::text::TextFile;

/// Line numbers are 0-based internally; UIs add 1 for display.
pub type LineNo = u64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error")]
    Io(#[from] std::io::Error),
    #[error("invalid pattern")]
    Pattern(#[from] regex::Error),
    #[error("invalid query: {0}")]
    Query(#[from] DslError),
    #[error("operation cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Shared progress / cancellation handle for long operations
/// (indexing, searching). Cheap to clone, safe to poll from a UI thread.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    inner: std::sync::Arc<ProgressInner>,
}

#[derive(Debug, Default)]
struct ProgressInner {
    done: std::sync::atomic::AtomicU64,
    total: std::sync::atomic::AtomicU64,
    cancelled: std::sync::atomic::AtomicBool,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_total(&self, total: u64) {
        self.inner.total.store(total, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn add(&self, n: u64) {
        self.inner.done.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }
    /// Fraction in `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        let total = self.inner.total.load(std::sync::atomic::Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let done = self.inner.done.load(std::sync::atomic::Ordering::Relaxed);
        (done as f64 / total as f64).min(1.0) as f32
    }
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }
}
