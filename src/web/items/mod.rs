//! Browsing and searching items, and reading one.

pub mod views;

use axum::{
    Router,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use maud::Markup;
use serde::Deserialize;

use super::{AppState, error::AppError};
use crate::{clock, feed, item};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/items", get(index))
        .route("/items/{id}", get(show))
}

/// Everything is text so that a bad value is ignored instead of refused, as
/// in feedthing2.
#[derive(Debug, Default, Deserialize)]
pub struct IndexQuery {
    pub feed_id: Option<String>,
    pub q: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<String>,
    pub offset: Option<String>,
}

/// The list's place, for its links.
pub struct Page<'a> {
    pub feed: Option<&'a feed::Feed>,
    pub search: &'a str,
    pub limit: i64,
    pub offset: i64,
    pub has_more: bool,
}

/// `GET /items`
async fn index(
    State(app): State<AppState>,
    Query(query): Query<IndexQuery>,
) -> Result<Markup, AppError> {
    let number = |text: &Option<String>| text.as_deref().and_then(|t| t.trim().parse::<i64>().ok());
    let search = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let filter = item::Filter {
        feed_id: number(&query.feed_id),
        search,
        since: number(&query.since),
        until: number(&query.until),
        limit: number(&query.limit)
            .unwrap_or(item::DEFAULT_PAGE)
            .clamp(1, item::MAX_PAGE),
        offset: number(&query.offset).unwrap_or(0).max(0),
    };
    let items = item::list(&app.db, &filter).await?;
    let feed = match filter.feed_id {
        Some(id) => feed::get(&app.db, id).await?,
        None => None,
    };
    let page = Page {
        feed: feed.as_ref(),
        search: search.unwrap_or(""),
        limit: filter.limit,
        offset: filter.offset,
        has_more: items.len() as i64 == filter.limit,
    };
    Ok(views::index(&items, &page, clock::now_ms(), &app.tz))
}

/// `GET /items/{id}`
async fn show(State(app): State<AppState>, Path(id): Path<String>) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::NotFound)?;
    let listed = item::get(&app.db, id).await?.ok_or(AppError::NotFound)?;
    Ok(views::show(&listed, clock::now_ms(), &app.tz).into_response())
}
