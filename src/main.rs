mod clock;
mod config;
mod content;
mod feed;
mod http_client;
mod item;
mod link;
mod parse;
mod poller;
mod scheduler;
mod shutdown;
mod web;

use std::{str::FromStr, time::Duration};

use anyhow::Context;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use tracing_subscriber::EnvFilter;

use crate::{config::Config, scheduler::Scheduler, web::AppState};

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
    let http = http_client::new(config.fetch_timeout);
    let stop = shutdown::on_signal();

    let scheduler = Scheduler {
        db: db.clone(),
        http: http.clone(),
    }
    .spawn(stop.clone());

    let app = web::router(AppState {
        db: db.clone(),
        tz: config.tz.clone(),
    });

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("listening on http://{}", config.bind);
    let server =
        axum::serve(listener, app).with_graceful_shutdown(shutdown::requested(stop.clone()));
    let deadline = async {
        shutdown::requested(stop.clone()).await;
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
    // It stops on the same signal; a poll in progress is dropped.
    if let Err(err) = scheduler.await {
        tracing::error!(%err, "the scheduler stopped with an error");
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
