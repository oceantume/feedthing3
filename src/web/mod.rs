//! Everything HTTP: routing, pages, and the plumbing they share. Handlers
//! turn requests into domain calls and render the results; SQL lives in the
//! domain modules, not here.

mod assets;
mod csrf;
mod error;
mod feeds;
mod health;
mod items;
mod layout;
mod push;
mod summary;
mod time;

use axum::{Router, middleware, routing::get};
use jiff::tz::TimeZone;
use sqlx::SqlitePool;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};

use crate::{enrich::Ai, push::Push};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub tz: TimeZone,
    /// For backfilling summaries, when configured.
    pub ai: Option<Ai>,
    /// For the notification bell, when configured.
    pub push: Option<Push>,
}

/// Every route in the app.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(summary::routes())
        .merge(feeds::routes())
        .merge(items::routes())
        .merge(push::routes())
        .nest_service("/static", assets::service())
        .layer(middleware::from_fn(csrf::same_origin_writes))
        // A panic answers 500 instead of dropping the connection...
        .layer(CatchPanicLayer::new())
        // ...and that, like every error without a page, gets one here.
        .layer(middleware::from_fn(error::page))
        .layer(TraceLayer::new_for_http())
        // Outside the layers above: checked every few seconds, it needs no
        // error page and would only clutter the request log.
        .route("/healthz", get(health::check))
        .with_state(state)
}
