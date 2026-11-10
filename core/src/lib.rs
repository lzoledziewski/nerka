//! Nerka core engine.
//!
//! The engine is UI-agnostic: the CLI, the desktop UI and (later) a server
//! all sit on top of the same building blocks (layout follows
//! `doc/nerka_architecture_implementation_spec_v_0.md` §10):
//!
//! * [`input`] — [`LogSource`]: random access to rows whatever the on-disk
//!   format: plain text, text records and JSON, DLT files, and DLT / SOME/IP
//!   / syslog inside pcap, pcapng and MF4 captures (`input::proto`), and
//!   protocols described by a definition (`.ksy`, `input::proto::ksy`);
//!   several files one after another or merged by time ([`open_many`],
//!   [`open_merged_by_time`]), messages of different protocols in one
//!   table (the mixed layout). [`open`] picks the reader from the content.
//! * [`engine`] — [`LineIndex`] (parallel line indexing) and [`search`]:
//!   filtering that yields the set of matching lines; [`search_within`]:
//!   search inside a filter result, per query.
//! * [`dsl`] — NFD, the filter language (`level >= ERROR AND raw ~ /SSL/`).
//! * [`event`] — lazily normalized fields (level, …) per line.
//! * [`signal`] — signals (`@name`): values captured from lines, their
//!   events and states; [`time`] — reading and showing times.
//! * [`lifecycle`] — an entity's lifecycles (boots), found from its lines.
//! * [`check`] — checks: issues and facts, overall and per lifecycle.
//! * [`session`] — a view's filter, search and colours, and what to rerun.
//! * [`bookmarks`] — bookmarks/notes persisted in a sidecar file.
//! * [`history`] — recently used filters; [`preset`], [`plot`] — saved
//!   presets and plots.
//! * [`pack`] — query packs (the search manager's library) and their
//!   expansion into plain NFD.
//! * [`atomic`] — [`write_atomic`] for every file nerka saves.
//! * [`bundle`] — settings bundles: export, import and upgrades of the
//!   analysis setup (packs, presets, plots, checks, signals).

pub mod atomic;
pub mod bookmarks;
pub mod bundle;
pub mod check;
pub mod dsl;
pub mod engine;
pub mod event;
pub mod history;
pub mod input;
pub mod lifecycle;
pub mod pack;
pub mod plot;
pub mod preset;
pub mod session;
pub mod signal;
pub mod time;

pub use atomic::write_atomic;
pub use bookmarks::Bookmarks;
pub use dsl::{DslError, Query};
pub use engine::index::LineIndex;
pub use engine::search::{Filter, Highlighter, Matcher, MatcherOptions, SearchResult, search, text_to_nfd};
pub use engine::within::{SearchHits, SearchSet, search_within};
pub use event::{Event, FieldDef, FieldId, FieldKind, FieldValue, Level};
pub use history::SearchHistory;
pub use input::concat::{open_many, open_many_by_time, open_merged_by_time};
pub use input::dlt::DltFile;
pub use input::dlt::fibex::{ArgType, Argument, Fibex, FibexError};
pub use input::text::TextFile;
pub use input::{Anchor, ColumnDef, LogSource, OpenOptions, SourceInfo, open, open_with, query_warnings};
pub use pack::{Library, Pack, PackError, PackQuery};
pub use session::{QueryContext, Session};
pub use signal::{
    Clock, QueryPart, SignalDef, SignalKind, SignalScope, SignalSeries, SignalValue, SignalView, Signals, ValueRef,
    query_signals,
};

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
    #[error("{0}")]
    Format(String),
    /// Files of different kinds (columns, fields) asked to open as one view.
    #[error("{0}")]
    MixedKinds(String),
    /// One of several files couldn't be opened: which one, and why (its
    /// reason included, so the message stands alone).
    #[error("{}: {}", path.display(), with_reason(reason))]
    Open { path: std::path::PathBuf, reason: Box<Error> },
}

impl Error {
    /// `e` as an error opening `path`; a cancel stays a cancel.
    pub fn opening(path: &std::path::Path, e: Error) -> Error {
        match e {
            Error::Cancelled | Error::Open { .. } => e,
            e => Error::Open { path: path.to_owned(), reason: Box::new(e) },
        }
    }
}

/// An error's text with its source's, as `I/O error (access denied)`.
fn with_reason(e: &Error) -> String {
    match std::error::Error::source(e) {
        Some(s) => format!("{e} ({s})"),
        None => e.to_string(),
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Per-user config directory for nerka (`%APPDATA%\nerka`, or
/// `$XDG_CONFIG_HOME/nerka`, `~/.config/nerka`), if one can be determined.
pub fn config_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("nerka"))
}

/// Per-user cache directory for nerka (`%LOCALAPPDATA%\nerka\cache`, or
/// `$XDG_CACHE_HOME/nerka`, `~/.cache/nerka`), if one can be determined:
/// where compressed captures are unpacked.
pub fn cache_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|b| PathBuf::from(b).join("nerka").join("cache"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|b| b.join("nerka"))
    }
}

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
