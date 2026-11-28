//! Starts the writers, the background tasks and the HTTP server, and stops
//! them in order on shutdown.

use super::{App, Keys, body, router};
use crate::{
    alert,
    logs::{
        self,
        store::{LogWriter, Overrides},
    },
    mcp, monitor, net, self_report, sourcemaps, store,
    token::Token,
    upload,
};
use axum::Router;
use std::{sync::Arc, time::Duration};

/// How late a missed heartbeat can be noticed.
const SWEEP_EVERY: Duration = Duration::from_secs(10);
/// How late a due notification can be sent.
const NOTIFY_EVERY: Duration = Duration::from_secs(1);
/// How late a due HTTP check can start.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// Starts the log writer, which reads the projects' log retention under `lock`.
fn log_writer(
    data: &std::path::Path,
    retention: crate::retention::Defaults,
    lock: crate::retention::Lock,
) -> anyhow::Result<LogWriter> {
    let projects = store::open(data)?;
    let overrides = Overrides::new(move || crate::retention::log_days(&projects), lock);
    LogWriter::start_with(
        logs::dir(data),
        retention.logs.get(),
        overrides,
        retention.hot_days.get(),
    )
}

/// Waits for each thread to end, and names the first that panicked.
fn join<const N: usize>(threads: [(&str, std::thread::JoinHandle<()>); N]) -> anyhow::Result<()> {
    for (name, thread) in threads {
        thread.join().map_err(|panic| {
            let why = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("no message");
            anyhow::anyhow!("{name} panicked: {why}")
        })?;
    }
    Ok(())
}

fn admin_routes(
    data: &std::path::Path,
    app: &App,
    token: Option<Token>,
    public_url: &url::Url,
    retention: crate::retention::Defaults,
    shared: crate::usage::Shared,
) -> anyhow::Result<Router> {
    // `/mcp` answers agent tokens without an admin token too; the scrub
    // settings and the upload answer the admin token only.
    let mcp = mcp::router(
        data.to_owned(),
        app.sender.clone(),
        token.clone(),
        public_url,
        retention,
        shared,
    )?;
    let admin = match token {
        Some(token) => mcp
            .merge(crate::scrub::settings::router(
                store::open(data)?,
                app.sender.clone(),
                token.clone(),
                app.keys.policies(),
            ))
            .merge(upload::router(
                Arc::clone(&app.sourcemaps),
                token,
                public_url,
            )),
        None => mcp,
    };
    Ok(admin)
}

type Tasks = [(&'static str, tokio::task::JoinHandle<()>); 5];

/// The usage flusher, stopped between two sends rather than aborted, so a
/// job it sent is never left without an answer.
struct Flusher {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Flusher {
    async fn stop(self) -> anyhow::Result<()> {
        drop(self.stop);
        self.task
            .await
            .map_err(|e| anyhow::anyhow!("usage flusher: {e}"))
    }
}

/// Spawns the tasks that queue jobs for the writer on a timer.
fn tasks(
    data: &std::path::Path,
    sender: &store::Sender,
    retention: crate::retention::Defaults,
    smtp: Option<alert::mail::Smtp>,
    public_url: &url::Url,
    usage: &crate::usage::Counter,
    log_writer: &LogWriter,
) -> anyhow::Result<(Tasks, Flusher)> {
    let sweeper = store::sweep_every(sender.clone(), SWEEP_EVERY, retention.errors);
    let checker = monitor::check::run(
        store::open(data)?,
        monitor::check::client(net::Reach::Any)?,
        sender.clone(),
        CHECK_EVERY,
    );
    let notifier = alert::deliver::run(
        store::open(data)?,
        alert::deliver::Transports::new(net::Reach::Any, smtp)?,
        sender.clone(),
        NOTIFY_EVERY,
        public_url.clone(),
    );
    let drops = crate::drops::reload_every(
        store::open(data)?,
        log_writer.drops.clone(),
        crate::drops::RELOAD_EVERY,
    );
    let quotas = crate::quota::reload_every(
        store::open(data)?,
        Arc::clone(usage.gate()),
        crate::quota::RELOAD_EVERY,
    );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let flusher = crate::usage::flush_every(
        usage.clone(),
        log_writer.log_days.clone(),
        sender.clone(),
        crate::usage::FLUSH_EVERY,
        stopped,
    );
    Ok((
        [
            ("sweeper", tokio::spawn(sweeper)),
            ("checker", tokio::spawn(checker)),
            ("notifier", tokio::spawn(notifier)),
            ("drop rules", tokio::spawn(drops)),
            ("quotas", tokio::spawn(quotas)),
        ],
        Flusher {
            stop,
            task: tokio::spawn(flusher),
        },
    ))
}

/// Stops the tasks. They hold senders, and the writer thread ends only when
/// every sender is gone. A notice in flight is sent again on the next start.
async fn stop(tasks: Tasks) -> anyhow::Result<()> {
    for (name, task) in tasks {
        task.abort();
        if let Err(e) = task.await
            && e.is_panic()
        {
            anyhow::bail!("{name} panicked");
        }
    }
    Ok(())
}

/// Davy's own lines on their way to Davy's own project, and the thread
/// that records them.
struct SelfReport {
    installed: Option<self_report::Installed>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Davy's own lines, queued from before the store and log writers start,
/// so what their first pass reports, such as a stray day of logs, is
/// recorded too. The queue holds [`self_report::QUEUE`] lines and drops and
/// counts the rest until the forwarder starts.
struct Early {
    installed: self_report::Installed,
    lines: std::sync::mpsc::Receiver<self_report::Line>,
    /// Davy's own project and the forwarder's connection, None when the
    /// project could not be opened.
    own: Option<(self_report::own::Own, rusqlite::Connection)>,
}

impl Early {
    /// Queues Davy's own lines from now on, and finds or makes Davy's own
    /// project in `data`. It runs before the writers start, so the store
    /// has one writer at a time.
    fn install(data: &std::path::Path) -> Self {
        let (sink, lines) = self_report::Sink::new(self_report::QUEUE);
        let installed = self_report::install(&sink);
        let own = self_report::own::open(data);
        Self {
            installed,
            lines,
            own,
        }
    }

    /// Sends the lines queued and to come to a new forwarder. Without the
    /// project, or when the forwarder does not start, which is reported,
    /// the lines go to stderr only.
    fn start(self, data: &std::path::Path, app: &App) -> SelfReport {
        let Self {
            installed,
            lines,
            own,
        } = self;
        let Some((own, db)) = own else {
            drop(installed);
            return SelfReport {
                installed: None,
                thread: None,
            };
        };
        let forwarder = self_report::Forwarder {
            data: data.to_owned(),
            own,
            db: std::sync::Mutex::new(db),
            logs: app.logs.clone(),
            usage: app.usage.clone(),
            policies: app.keys.policies(),
            sender: app.sender.clone(),
            budget: app.budget.clone(),
        };
        let thread = forwarder
            .start(lines)
            .inspect_err(|e| self_report::stderr("self report", format_args!("not started: {e}")))
            .ok();
        SelfReport {
            installed: Some(installed),
            thread,
        }
    }
}

impl SelfReport {
    /// Records what is queued and stops. Lines from now on go to stderr
    /// only. A forwarder that panicked is reported and does not fail the
    /// shutdown.
    async fn stop(self) {
        drop(self.installed);
        let Some(thread) = self.thread else {
            return;
        };
        let joined = tokio::task::spawn_blocking(move || join([("self report", thread)])).await;
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(e)) => self_report::stderr("self report", e),
            Err(e) => self_report::stderr("self report", e),
        }
    }
}

/// Serves ingest, and with a token `/mcp` and the source map upload.
/// `public_url` is where clients reach this server. It serves the data
/// directory of `claim`, which the caller holds until this returns, so no
/// restore runs under it.
pub async fn serve(
    claim: &crate::backup::swap::Claim,
    listen: &str,
    public_url: &url::Url,
    retention: crate::retention::Defaults,
    token: Option<Token>,
    smtp: Option<alert::mail::Smtp>,
    self_report: bool,
) -> anyhow::Result<()> {
    let data = claim.data();
    let early = self_report.then(|| Early::install(data));
    let writer = store::Writer::start(store::open(data)?, 1024);
    let log_writer = log_writer(data, retention, writer.retention.clone().with_dir(data))?;
    crate::drops::reload(&store::open(data)?, &log_writer.drops)?;
    let app = App {
        keys: Arc::new(Keys::new(store::open(data)?)),
        sender: writer.sender.clone(),
        logs: log_writer.queue.clone(),
        sourcemaps: Arc::new(sourcemaps::Store::open(data)),
        budget: body::Budget::new(body::MEMORY),
        usage: crate::usage::Counter::default(),
    };
    let usage = app.usage.clone();
    crate::quota::start(&store::open(data)?, usage.gate(), crate::time::now_ns())?;
    let reporter = early.map(|early| early.start(data, &app));
    // One disk walk a minute and one limit on usage reads, for the MCP
    // tool and the web UI together, and the gate the MCP tool asks what
    // it refuses now.
    let shared = crate::usage::Shared::with_gate(Arc::clone(usage.gate()));
    let ui = crate::ui::serve(
        data,
        writer.ui.clone(),
        app.budget.clone(),
        public_url,
        retention,
        shared.clone(),
    )?;
    let admin = admin_routes(data, &app, token, public_url, retention, shared)?;
    let routes = router(app, admin.merge(ui));
    let listener = tokio::net::TcpListener::bind(listen).await?;
    crate::ready::notify()?;
    let (tasks, flusher) = tasks(
        data,
        &writer.sender,
        retention,
        smtp,
        public_url,
        &usage,
        &log_writer,
    )?;
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = axum::serve(listener, routes)
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            #[cfg(not(unix))]
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await;
    stop(tasks).await?;
    if let Some(reporter) = reporter {
        reporter.stop().await;
    }
    flusher.stop().await?;
    shut_down(log_writer, &usage, writer).await?;
    result?;
    Ok(())
}

/// Stops the log writer, then the writer, once the tasks and the server
/// are stopped. The counts go out before the log writer stops, as its stop
/// commits, settles and waits for merges and can take longer than a
/// supervisor waits before a kill. They go out once more after it, with
/// the log writer's last counts and cut marks.
async fn shut_down(
    log_writer: LogWriter,
    usage: &crate::usage::Counter,
    writer: store::Writer,
) -> anyhow::Result<()> {
    let log_days = log_writer.log_days;
    if let Err(e) = crate::usage::flush(usage, &log_days, &writer.sender).await {
        crate::usage::warn_held(usage, "the counts since the last flush were not stored", &e);
    }
    drop(log_writer.queue);
    let logs_stopped = join([("log writer", log_writer.thread)]);
    if let Err(e) = crate::usage::flush(usage, &log_days, &writer.sender).await {
        crate::usage::warn_held(usage, "the log writer's last counts were not stored", &e);
    }
    drop(writer.sender);
    drop(writer.ui);
    join([("writer", writer.thread)])?;
    logs_stopped
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
