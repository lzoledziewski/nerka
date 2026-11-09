//! A view's queries and what they depend on, without a UI: the filter, the
//! search inside its result and the line colours, run against one source
//! with one [`QueryContext`] (signals and bookmarks).
//!
//! The session decides what is out of date and hands out [`Task`]s; the
//! caller runs them wherever it likes (a thread per task in the GUI, in
//! place in tests or the CLI) and gives the result back with
//! [`Session::finish`]. A task that was overtaken (its query or the
//! context changed while it ran) is ignored when it finishes, so the
//! caller may cancel it or not.
//!
//! Order: the filter first; the search and the colours run over its result
//! (every line without a filter) once it is in. When the bookmarks or the
//! signals change, only the queries that use them run again, with the new
//! ones: the filter that ran, not a box's text that may have been edited
//! since.

use crate::engine::search::{Filter, SearchResult, search};
use crate::engine::within::{SearchHits, SearchSet, search_within};
use crate::input::LogSource;
use crate::signal::Signals;
use crate::{Bookmarks, DslError, LineNo, Progress, Query, Result};
use std::sync::Arc;

/// What queries are run against besides the source: the view's signals
/// (`@name`) and bookmarks (`bookmark`).
#[derive(Debug, Clone, Default)]
pub struct QueryContext {
    pub signals: Signals,
    pub bookmarks: Arc<Bookmarks>,
}

impl QueryContext {
    /// Compile NFD for `source`: fields, signals and bookmarks resolved.
    pub fn compile(&self, nfd: &str, source: &dyn LogSource) -> std::result::Result<Query, DslError> {
        Ok(Query::compile_in(nfd, source.schema(), &self.signals)?.with_bookmarks(&self.bookmarks))
    }

    /// `f` compiled again with this context (a regex needs none).
    pub fn rebind(&self, f: &Filter, source: &dyn LogSource) -> std::result::Result<Filter, DslError> {
        Ok(match f {
            Filter::Regex(m) => Filter::Regex(m.clone()),
            Filter::Query(q) => Filter::Query(self.compile(q.source(), source)?),
        })
    }
}

/// Whether a filter's result can change with the signals.
fn uses_signals(f: &Filter) -> bool {
    matches!(f, Filter::Query(q) if !q.signal_names().is_empty())
}

fn set_uses(set: &SearchSet, test: fn(&Filter) -> bool) -> bool {
    set.include.iter().chain(&set.exclude).any(test)
}

/// The three queries of a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Which lines the view shows.
    Filter,
    /// Hits among the filter's lines.
    Search,
    /// Line colours among the filter's lines.
    Colors,
}

/// One query of a session: what to run, its result, and whether that is
/// out of date.
#[derive(Debug)]
struct Slot<Q, R> {
    query: Option<Q>,
    result: Option<Arc<R>>,
    /// The ticket of the task running for it.
    running: Option<u64>,
    /// Its result is out of date: a task has to run.
    stale: bool,
}

impl<Q, R> Default for Slot<Q, R> {
    fn default() -> Self {
        Slot { query: None, result: None, running: None, stale: false }
    }
}

impl<Q, R> Slot<Q, R> {
    fn set(&mut self, q: Option<Q>) {
        self.query = q;
        self.running = None;
        self.stale = self.query.is_some();
        if self.query.is_none() {
            self.result = None;
        }
    }

    fn dirty(&mut self) {
        if self.query.is_some() {
            self.stale = true;
            self.running = None;
        }
    }

    fn busy(&self) -> bool {
        self.stale || self.running.is_some()
    }
}

/// Work for the caller to run: [`run`](Task::run) it, then hand the
/// outcome to [`Session::finish`] with its [`ticket`](Task::ticket).
pub struct Task {
    pub stage: Stage,
    pub ticket: u64,
    source: Arc<dyn LogSource>,
    work: Work,
}

enum Work {
    Filter(Arc<Filter>),
    /// A search set over the filter's lines (all lines without a filter).
    Within(Arc<SearchSet>, Option<Arc<SearchResult>>),
}

/// What a task found.
#[derive(Debug)]
pub enum Output {
    Filter(SearchResult),
    Hits(SearchHits),
}

impl Task {
    /// Do the work (on any thread). Checks `progress` for cancelling.
    pub fn run(&self, progress: &Progress) -> Result<Output> {
        match &self.work {
            Work::Filter(f) => search(&*self.source, f, progress).map(Output::Filter),
            Work::Within(set, within) => {
                let lines = within.as_ref().map(|r| r.lines.as_slice());
                search_within(&*self.source, lines, set, progress).map(Output::Hits)
            }
        }
    }
}

/// What [`Session::finish`] did with a task's outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum Finished {
    /// The result is the stage's now.
    Done(Stage),
    /// Overtaken by a change, or cancelled: nothing changed.
    Ignored,
}

/// One view's filter, search and colours over a source.
#[derive(Default)]
pub struct Session {
    source: Option<Arc<dyn LogSource>>,
    cx: QueryContext,
    filter: Slot<Arc<Filter>, SearchResult>,
    search: Slot<Arc<SearchSet>, SearchHits>,
    colors: Slot<Arc<SearchSet>, SearchHits>,
    next_ticket: u64,
}

impl Session {
    pub fn new() -> Session {
        Session::default()
    }

    pub fn source(&self) -> Option<&Arc<dyn LogSource>> {
        self.source.as_ref()
    }

    pub fn context(&self) -> &QueryContext {
        &self.cx
    }

    /// Compile NFD for this session's source and context.
    pub fn compile(&self, nfd: &str) -> std::result::Result<Query, DslError> {
        match &self.source {
            Some(s) => self.cx.compile(nfd, &**s),
            None => Ok(Query::compile(nfd)?.with_signals(&self.cx.signals)?.with_bookmarks(&self.cx.bookmarks)),
        }
    }

    /// A new source (the file was read, or read again): every result goes
    /// and every query runs again on it.
    pub fn set_source(&mut self, source: Arc<dyn LogSource>) {
        self.source = Some(source);
        self.filter.result = None;
        self.search.result = None;
        self.colors.result = None;
        self.filter.dirty();
        self.search.dirty();
        self.colors.dirty();
    }

    /// Filter by `f` (bookmarks given to it here), or show every line.
    pub fn set_filter(&mut self, f: Option<Filter>) {
        let f = f.map(|f| Arc::new(f.with_bookmarks(&self.cx.bookmarks)));
        let none = f.is_none();
        self.filter.set(f);
        if none {
            // Every line now: what runs over the lines runs again.
            self.search.dirty();
            self.colors.dirty();
        }
    }

    /// Search for `set` among the filter's lines, or stop searching.
    pub fn set_search(&mut self, set: Option<SearchSet>) {
        self.search.set(set.map(|s| Arc::new(self.bind_set(s))));
    }

    /// Colour the filter's lines that `set` matches, or none.
    pub fn set_colors(&mut self, set: Option<SearchSet>) {
        self.colors.set(set.map(|s| Arc::new(self.bind_set(s))));
    }

    fn bind_set(&self, s: SearchSet) -> SearchSet {
        let b = |fs: Vec<Filter>| fs.into_iter().map(|f| f.with_bookmarks(&self.cx.bookmarks)).collect();
        SearchSet { include: b(s.include), exclude: b(s.exclude) }
    }

    /// The bookmarks changed: what uses `bookmark` runs again with them.
    pub fn set_bookmarks(&mut self, marks: Bookmarks) {
        if *self.cx.bookmarks == marks {
            return;
        }
        self.cx.bookmarks = Arc::new(marks);
        let marks = self.cx.bookmarks.clone();
        if let Some(f) = self.filter.query.clone().filter(|f| f.uses_bookmarks()) {
            self.filter.query = Some(Arc::new((*f).clone().with_bookmarks(&marks)));
            self.filter.dirty();
        }
        for slot in [&mut self.search, &mut self.colors] {
            if let Some(s) = slot.query.clone().filter(|s| set_uses(s, Filter::uses_bookmarks)) {
                let b = |fs: &[Filter]| fs.iter().map(|f| f.clone().with_bookmarks(&marks)).collect();
                slot.query = Some(Arc::new(SearchSet { include: b(&s.include), exclude: b(&s.exclude) }));
                slot.dirty();
            }
        }
    }

    /// The signals changed: what uses them is compiled again with the new
    /// ones and runs again. A query that no longer compiles (its signal is
    /// gone) is dropped; the errors are returned per stage.
    pub fn set_signals(&mut self, signals: Signals) -> Vec<(Stage, DslError)> {
        self.cx.signals = signals;
        let mut errors = Vec::new();
        let Some(source) = self.source.clone() else { return errors };
        if let Some(f) = self.filter.query.clone().filter(|f| uses_signals(f)) {
            match self.cx.rebind(&f, &*source) {
                Ok(f) => {
                    self.filter.query = Some(Arc::new(f));
                    self.filter.dirty();
                }
                Err(e) => {
                    errors.push((Stage::Filter, e));
                    self.set_filter(None);
                }
            }
        }
        for (stage, slot) in [(Stage::Search, &mut self.search), (Stage::Colors, &mut self.colors)] {
            let Some(s) = slot.query.clone().filter(|s| set_uses(s, uses_signals)) else { continue };
            let rebind =
                |fs: &[Filter]| fs.iter().map(|f| self.cx.rebind(f, &*source)).collect::<std::result::Result<_, _>>();
            match rebind(&s.include).and_then(|include| Ok(SearchSet { include, exclude: rebind(&s.exclude)? })) {
                Ok(set) => {
                    slot.query = Some(Arc::new(set));
                    slot.dirty();
                }
                Err(e) => {
                    errors.push((stage, e));
                    slot.set(None);
                }
            }
        }
        errors
    }

    /// The stages [`tasks`](Self::tasks) would start now (it isn't called).
    pub fn pending(&self) -> Vec<Stage> {
        if self.source.is_none() {
            return Vec::new();
        }
        if self.filter.busy() {
            return if self.filter.stale { vec![Stage::Filter] } else { Vec::new() };
        }
        [(Stage::Search, self.search.stale), (Stage::Colors, self.colors.stale)]
            .into_iter()
            .filter_map(|(st, stale)| stale.then_some(st))
            .collect()
    }

    /// The tasks to start now; each must be run and finished (or it is
    /// waited for). A stage with a task running gets a new one
    /// only after a change; the search and the colours wait for the filter.
    pub fn tasks(&mut self) -> Vec<Task> {
        let Some(source) = self.source.clone() else { return Vec::new() };
        let mut out = Vec::new();
        if self.filter.stale {
            let f = self.filter.query.clone().expect("only a set filter is stale");
            out.push(self.start(Stage::Filter, &source, Work::Filter(f)));
        }
        if self.filter.busy() {
            return out;
        }
        let within = self.filter.result.clone();
        if self.search.stale {
            let s = self.search.query.clone().expect("set");
            out.push(self.start(Stage::Search, &source, Work::Within(s, within.clone())));
        }
        if self.colors.stale {
            let s = self.colors.query.clone().expect("set");
            out.push(self.start(Stage::Colors, &source, Work::Within(s, within)));
        }
        out
    }

    fn start(&mut self, stage: Stage, source: &Arc<dyn LogSource>, work: Work) -> Task {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let slot_running = match stage {
            Stage::Filter => (&mut self.filter.stale, &mut self.filter.running),
            Stage::Search => (&mut self.search.stale, &mut self.search.running),
            Stage::Colors => (&mut self.colors.stale, &mut self.colors.running),
        };
        *slot_running.0 = false;
        *slot_running.1 = Some(ticket);
        Task { stage, ticket, source: source.clone(), work }
    }

    /// The caller dropped the tasks it was running (cancelled them): those
    /// stages are due again with the next [`tasks`](Self::tasks).
    pub fn requeue_running(&mut self) {
        for (stale, running) in [
            (&mut self.filter.stale, &mut self.filter.running),
            (&mut self.search.stale, &mut self.search.running),
            (&mut self.colors.stale, &mut self.colors.running),
        ] {
            if running.take().is_some() {
                *stale = true;
            }
        }
    }

    /// Take a task's outcome. A filter result makes the search and the
    /// colours run again over it. An error (not a cancel) is returned so
    /// the caller can show it; the stage then has no result, and a failed
    /// filter takes the search's and the colours' results along (they were
    /// found inside the previous filter's lines) without running them again
    /// over lines nobody chose. It runs again when its query changes.
    pub fn finish(&mut self, ticket: u64, outcome: Result<Output>) -> Result<Finished> {
        let stage = if self.filter.running == Some(ticket) {
            Stage::Filter
        } else if self.search.running == Some(ticket) {
            Stage::Search
        } else if self.colors.running == Some(ticket) {
            Stage::Colors
        } else {
            return Ok(Finished::Ignored);
        };
        let out = match outcome {
            Ok(o) => o,
            Err(crate::Error::Cancelled) => {
                self.slot_running(stage, None);
                return Ok(Finished::Ignored);
            }
            Err(e) => {
                self.slot_running(stage, None);
                match stage {
                    Stage::Filter => {
                        self.filter.result = None;
                        self.search.result = None;
                        self.colors.result = None;
                    }
                    Stage::Search => self.search.result = None,
                    Stage::Colors => self.colors.result = None,
                }
                return Err(e);
            }
        };
        self.slot_running(stage, None);
        match (stage, out) {
            (Stage::Filter, Output::Filter(r)) => {
                self.filter.result = Some(Arc::new(r));
                self.search.dirty();
                self.colors.dirty();
            }
            (Stage::Search, Output::Hits(h)) => self.search.result = Some(Arc::new(h)),
            (Stage::Colors, Output::Hits(h)) => self.colors.result = Some(Arc::new(h)),
            _ => unreachable!("a stage's task gives its kind of output"),
        }
        Ok(Finished::Done(stage))
    }

    fn slot_running(&mut self, stage: Stage, v: Option<u64>) {
        match stage {
            Stage::Filter => self.filter.running = v,
            Stage::Search => self.search.running = v,
            Stage::Colors => self.colors.running = v,
        }
    }

    /// The filter that ran or runs (with the context bound), if any.
    pub fn filter(&self) -> Option<&Arc<Filter>> {
        self.filter.query.as_ref()
    }

    pub fn search_set(&self) -> Option<&Arc<SearchSet>> {
        self.search.query.as_ref()
    }

    pub fn colors_set(&self) -> Option<&Arc<SearchSet>> {
        self.colors.query.as_ref()
    }

    /// The lines the filter kept, once in (`None`: no filter, or not yet).
    pub fn result(&self) -> Option<&Arc<SearchResult>> {
        self.filter.result.as_ref()
    }

    pub fn hits(&self) -> Option<&Arc<SearchHits>> {
        self.search.result.as_ref()
    }

    pub fn colors(&self) -> Option<&Arc<SearchHits>> {
        self.colors.result.as_ref()
    }

    /// Whether `stage` has work waiting or running.
    pub fn is_busy(&self, stage: Stage) -> bool {
        match stage {
            Stage::Filter => self.filter.busy(),
            Stage::Search => self.search.busy() || (self.search.query.is_some() && self.filter.busy()),
            Stage::Colors => self.colors.busy() || (self.colors.query.is_some() && self.filter.busy()),
        }
    }

    /// Run every task in place until nothing is left (tests, the CLI).
    pub fn run_all(&mut self, progress: &Progress) -> Result<()> {
        loop {
            let tasks = self.tasks();
            if tasks.is_empty() {
                return Ok(());
            }
            for t in tasks {
                let out = t.run(progress);
                self.finish(t.ticket, out)?;
            }
        }
    }

    /// The main view's rows: the filter's lines, or every line.
    pub fn rows(&self) -> LineNo {
        match (&self.filter.result, &self.source) {
            (Some(r), _) => r.lines.len() as LineNo,
            (None, Some(s)) => s.line_count(),
            (None, None) => 0,
        }
    }
}

#[cfg(test)]
mod tests;
