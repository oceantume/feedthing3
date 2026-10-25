mod clock;
mod config;
mod feed;
mod http_client;
mod web;

use std::{str::FromStr, time::Duration};

use anyhow::Context;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use tracing_subscriber::EnvFilter;

use crate::{config::Config, web::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "feedthing=debug,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let db = open(&config.database_url).await?;

    let app = web::router(AppState {
        db: db.clone(),
        tz: config.tz.clone(),
    });

    let stopping = stop_signal();

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("listening on http://{}", config.bind);
    let server = axum::serve(listener, app).with_graceful_shutdown(stopped(stopping.clone()));
    let deadline = async {
        stopped(stopping.clone()).await;
        tokio::time::sleep(SHUTDOWN_GRACE).await;
    };
    tokio::select! {
        served = server.into_future() => served?,
        () = deadline => {
            // Not closing the pool: a stuck request may hold a connection.
            // SQLite finishes the checkpoint from the WAL on the next start.
            tracing::warn!("requests still open {SHUTDOWN_GRACE:?} after the stop signal; exiting anyway");
            return Ok(());
        }
    }
    // Checkpoints the WAL into the database file.
    db.close().await;
    Ok(())
}

/// Opens (or creates) the database and brings its schema up to date.
async fn open(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let db = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .context("opening database")?;
    sqlx::migrate!()
        .run(&db)
        .await
        .context("running migrations")?;
    Ok(db)
}

/// How long requests in flight at shutdown get to finish. The box gives the
/// whole process 20 seconds before it kills it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Turns true on Ctrl-C or SIGTERM (what Docker sends). Everything that runs
/// until shutdown watches it: the server and the scheduler.
fn stop_signal() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    let mut terminate = signal(SignalKind::terminate()).expect("installing the SIGTERM handler");
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
        tracing::info!("shutting down");
        tx.send_replace(true);
    });
    rx
}

/// Resolves once [`stop_signal`] has fired.
async fn stopped(mut stopping: watch::Receiver<bool>) {
    // An error means the sender is gone, which only happens after it fired.
    let _ = stopping.wait_for(|stop| *stop).await;
}
