//! Starts the writers, the background tasks and the HTTP server, and stops
//! them in order on shutdown.

use super::{App, Keys, body, router};
use crate::{
    alert,
    logs::{
        self,
        store::{LogWriter, Overrides},
    },
    mcp, monitor, net, sourcemaps, store,
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
    LogWriter::start(logs::dir(data), retention.logs.get(), overrides)
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
    sender: &store::Sender,
    token: Option<Token>,
    public_url: &url::Url,
    maps: Arc<sourcemaps::Store>,
    policies: Arc<crate::scrub::settings::Cache>,
    retention: crate::retention::Defaults,
) -> anyhow::Result<Router> {
    let admin = match token {
        Some(token) => mcp::router(
            data.to_owned(),
            sender.clone(),
            token.clone(),
            public_url,
            retention,
        )
        .merge(crate::scrub::settings::router(
            store::open(data)?,
            sender.clone(),
            token.clone(),
            policies,
        ))
        .merge(upload::router(maps, token, public_url)),
        None => Router::new(),
    };
    Ok(admin)
}

/// Serves ingest, and with a token `/mcp` and the source map upload.
/// `public_url` is where clients reach this server.
pub async fn serve(
    data: &std::path::Path,
    listen: &str,
    public_url: &url::Url,
    retention: crate::retention::Defaults,
    token: Option<Token>,
    smtp: Option<alert::mail::Smtp>,
) -> anyhow::Result<()> {
    let writer = store::Writer::start(store::open(data)?, 1024);
    let log_writer = log_writer(data, retention, writer.retention.clone().with_dir(data))?;
    let app = App {
        keys: Arc::new(Keys::new(store::open(data)?)),
        sender: writer.sender.clone(),
        logs: log_writer.queue.clone(),
        sourcemaps: Arc::new(sourcemaps::Store::open(data)),
        budget: body::Budget::new(body::MEMORY),
    };
    let maps = Arc::clone(&app.sourcemaps);
    let ui = crate::ui::serve(
        data,
        writer.ui.clone(),
        app.budget.clone(),
        public_url,
        retention,
    )?;
    let policies = app.keys.policies();
    let admin = admin_routes(
        data,
        &writer.sender,
        token,
        public_url,
        maps,
        policies,
        retention,
    )?;
    let routes = router(app, admin.merge(ui));
    let listener = tokio::net::TcpListener::bind(listen).await?;
    crate::ready::notify()?;
    let sweeper = store::sweep_every(writer.sender.clone(), SWEEP_EVERY, retention.errors);
    let sweeper = tokio::spawn(sweeper);
    let checker = tokio::spawn(monitor::check::run(
        store::open(data)?,
        monitor::check::client(net::Reach::Any)?,
        writer.sender.clone(),
        CHECK_EVERY,
    ));
    let notifier = tokio::spawn(alert::deliver::run(
        store::open(data)?,
        alert::deliver::Transports::new(net::Reach::Any, smtp)?,
        writer.sender.clone(),
        NOTIFY_EVERY,
        public_url.clone(),
    ));
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
    // These tasks hold senders, and the writer thread ends only when every
    // sender is gone. A notice in flight is sent again on the next start.
    for (name, task) in [
        ("sweeper", sweeper),
        ("checker", checker),
        ("notifier", notifier),
    ] {
        task.abort();
        if let Err(e) = task.await
            && e.is_panic()
        {
            anyhow::bail!("{name} panicked");
        }
    }
    drop(writer.sender);
    drop(writer.ui);
    drop(log_writer.queue);
    join([("writer", writer.thread), ("log writer", log_writer.thread)])?;
    result?;
    Ok(())
}
