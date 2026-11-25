//! Davy's own warnings and errors. Every line Davy prints to stderr about
//! its own work goes through [`warn`], [`error`] or [`info`]: the line goes
//! to stderr as before, and while `davy serve` runs with self-reporting
//! on, also into Davy's own project in its store, see [`own`], so an agent
//! finds it with `search_logs` like any other log.
//!
//! Recording a line must never cost a request or Davy itself. The line is
//! offered to a bounded queue without waiting, a line whose pattern repeats
//! is counted instead of recorded, and a failure while recording Davy's own
//! lines goes to stderr only. "Self-monitoring" in docs/design.md has the
//! rules.

use crate::logs::{LogLevel, patterns};
use std::{
    cell::Cell,
    fmt::Display,
    io::Write as _,
    marker::PhantomData,
    sync::{
        Arc, Mutex, PoisonError, RwLock,
        atomic::{AtomicI64, AtomicU64, Ordering},
        mpsc,
    },
};

pub mod crash;
mod forward;
mod limit;
pub mod own;
pub mod status;
pub use forward::Forwarder;
pub use limit::{Admit, Limiter, MAX_KEYS, OVERFLOW, PER_WINDOW, WINDOW_NS};

/// The slug of Davy's own project, unless a project a person made has it,
/// see [`own::slugs`].
pub const PROJECT: &str = "davy";
/// The service of every record Davy makes of itself.
pub const SERVICE: &str = "davy";
/// Days of logs the project keeps when Davy creates it.
pub const LOG_DAYS: u32 = 30;
/// Lines that wait for the forwarder. More are dropped and counted.
pub const QUEUE: usize = 1024;
/// The longest message recorded, in bytes. stderr gets it whole.
pub const MAX_MESSAGE: usize = 8 * 1024;

/// One of Davy's own lines on its way to the forwarder.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub at_ns: i64,
    pub level: LogLevel,
    pub component: &'static str,
    pub message: String,
    /// Lines of the same component and pattern counted instead of
    /// recorded since the last one recorded.
    pub suppressed: u64,
    /// Lines of any kind not recorded since the last line queued: lines
    /// dropped because the queue was full, with the lines they counted,
    /// and lines counted in a window the limiter forgot.
    pub dropped: u64,
}

/// Prints `component: message` to stderr and records it at `level`.
pub fn log(level: LogLevel, component: &'static str, message: impl Display) {
    note(level, component, &message.to_string());
}

/// A change or event an operator may want to look up later, such as a
/// retention change.
pub fn info(component: &'static str, message: impl Display) {
    note(LogLevel::Info, component, &message.to_string());
}

/// Something did not work and Davy carries on, such as a day of logs that
/// retention skipped until its next pass.
pub fn warn(component: &'static str, message: impl Display) {
    note(LogLevel::Warn, component, &message.to_string());
}

/// Something did not work and data or work was lost, such as logs that
/// were not written.
pub fn error(component: &'static str, message: impl Display) {
    note(LogLevel::Error, component, &message.to_string());
}

/// Prints `component: message` to stderr, and nothing else. For a failure
/// while recording Davy's own lines, which recording would repeat. A
/// stderr that cannot be written is ignored: Davy has nowhere else to say
/// it, and `eprintln!` would panic.
pub fn stderr(component: &str, message: impl Display) {
    let mut err = std::io::stderr().lock();
    writeln!(err, "{component}: {message}").unwrap_or_default();
}

fn note(level: LogLevel, component: &'static str, message: &str) {
    stderr(component, message);
    if quiet_now() {
        return;
    }
    let Some(sink) = installed() else {
        return;
    };
    let _quiet = quiet();
    if sink.offer(level, component, message, crate::time::now_ns()) == Offer::Closed {
        closed(&sink);
    }
}

/// Stops offering lines to `sink`, whose forwarder is gone, and says so
/// once: a forwarder that ended without a stop has panicked, and its crash
/// file waits for the next start.
fn closed(sink: &Arc<Sink>) {
    let mut slot = SINK.write().unwrap_or_else(PoisonError::into_inner);
    if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, sink)) {
        *slot = None;
        drop(slot);
        stderr(
            "self report",
            "Davy's own lines are no longer recorded, as the thread that records them has stopped; they go to stderr only until the next start",
        );
    }
}

thread_local! {
    static QUIET: Cell<u32> = const { Cell::new(0) };
}

/// While it is held, the lines of this thread go to stderr only. The code
/// that records Davy's own lines holds one, so a failure there is never
/// recorded in turn, and a line cannot recurse into itself.
pub struct Quiet(PhantomData<*const ()>);

pub fn quiet() -> Quiet {
    // A thread that is ending has no thread locals left, and its lines go
    // to stderr only, see `quiet_now`.
    QUIET
        .try_with(|q| q.set(q.get().saturating_add(1)))
        .unwrap_or_default();
    Quiet(PhantomData)
}

impl Drop for Quiet {
    fn drop(&mut self) {
        QUIET
            .try_with(|q| q.set(q.get().saturating_sub(1)))
            .unwrap_or_default();
    }
}

fn quiet_now() -> bool {
    QUIET.try_with(|q| q.get() > 0).unwrap_or(true)
}

/// What became of a line offered to a [`Sink`].
#[derive(Debug, PartialEq, Eq)]
pub enum Offer {
    Queued,
    /// Its pattern was recorded [`PER_WINDOW`] times in this window, so
    /// it is counted with the next one recorded.
    Limited,
    /// The queue was full, so it is counted in the next line queued.
    Dropped,
    /// The forwarder is gone.
    Closed,
}

/// Where Davy's lines wait for the forwarder.
pub struct Sink {
    queue: mpsc::SyncSender<Line>,
    limiter: Mutex<Limiter>,
    dropped: AtomicU64,
}

impl Sink {
    /// A sink whose queue holds `capacity` lines, and the receiver the
    /// forwarder reads them from.
    pub fn new(capacity: usize) -> (Arc<Self>, mpsc::Receiver<Line>) {
        let (queue, lines) = mpsc::sync_channel(capacity);
        let sink = Self {
            queue,
            limiter: Mutex::new(Limiter::default()),
            dropped: AtomicU64::new(0),
        };
        (Arc::new(sink), lines)
    }

    /// Queues the line unless its pattern is over its limit or the queue
    /// is full. Never waits.
    pub fn offer(
        &self,
        level: LogLevel,
        component: &'static str,
        message: &str,
        at_ns: i64,
    ) -> Offer {
        let key = (component, patterns::of(message).id);
        let (admitted, lost) = {
            let mut limiter = self.limiter.lock().unwrap_or_else(PoisonError::into_inner);
            (limiter.admit(key, at_ns), limiter.take_lost())
        };
        // Lines counted in a window the limiter forgot are not recorded
        // either, so the next line queued counts them as dropped.
        self.dropped.fetch_add(lost, Ordering::Relaxed);
        let suppressed = match admitted {
            Admit::Record { suppressed } => suppressed,
            Admit::Count => return Offer::Limited,
        };
        let line = Line {
            at_ns,
            level,
            component,
            message: capped(message, MAX_MESSAGE).to_owned(),
            suppressed,
            dropped: self.dropped.swap(0, Ordering::Relaxed),
        };
        match self.queue.try_send(line) {
            Ok(()) => Offer::Queued,
            Err(mpsc::TrySendError::Full(line)) => {
                // The lines this one counted are lost with it.
                self.drop_lines(line.dropped + line.suppressed + 1);
                Offer::Dropped
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Offer::Closed,
        }
    }

    /// Counts `n` lines that were not queued, and says so on stderr when
    /// they are the first since a line was queued.
    fn drop_lines(&self, n: u64) {
        if self.dropped.fetch_add(n, Ordering::Relaxed) == 0 {
            stderr(
                "self report",
                "Davy's own lines are dropped until its queue drains, and the next one recorded counts them in davy.dropped",
            );
        }
    }
}

/// The first `max` bytes of `s`, cut at a character boundary.
pub fn capped(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

static SINK: RwLock<Option<Arc<Sink>>> = RwLock::new(None);
static PROJECT_ID: AtomicI64 = AtomicI64::new(0);

/// Sends every line to `sink` until the guard drops.
pub fn install(sink: &Arc<Sink>) -> Installed {
    *SINK.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(sink));
    Installed(Arc::clone(sink))
}

/// Lines go to the sink while this lives.
pub struct Installed(Arc<Sink>);

impl Drop for Installed {
    fn drop(&mut self) {
        let mut slot = SINK.write().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.0)) {
            *slot = None;
        }
    }
}

fn installed() -> Option<Arc<Sink>> {
    SINK.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The id of Davy's own project once `davy serve` has found or made it in
/// this process.
pub fn project_id() -> Option<i64> {
    let id = PROJECT_ID.load(Ordering::Relaxed);
    (id != 0).then_some(id)
}

fn set_project_id(id: i64) {
    PROJECT_ID.store(id, Ordering::Relaxed);
}

/// Held by every test that installs a sink, as there is one per process.
#[cfg(test)]
pub(crate) fn install_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "self_report_tests.rs"]
mod tests;
