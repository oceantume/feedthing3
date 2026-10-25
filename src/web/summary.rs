//! The home page: what came in since the start of yesterday.

use axum::{Router, extract::State, routing::get};
use maud::{Markup, html};

use super::{
    AppState,
    error::AppError,
    items::views::{description_and_tags, title_of},
    layout, time,
};
use crate::{clock, item, link};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(show))
}

/// `GET /`
///
/// Grouped by feed. Titles go straight to the original page, since that's
/// what reading the summary is for.
async fn show(State(app): State<AppState>) -> Result<Markup, AppError> {
    let now = clock::now_ms();
    let since = time::start_of_yesterday(now, &app.tz);
    let groups = item::grouped_by_feed(&app.db, since, now).await?;
    Ok(layout::page(
        "Summary",
        html! {
            h1 { "Recent items" }
            @if groups.is_empty() {
                p.muted { "No recent items." }
            }
            @for (feed_name, items) in &groups {
                section.section {
                    h2 { (feed_name) }
                    ul.list.items {
                        @for listed in items {
                            @let item = &listed.item;
                            li {
                                a.item-title href=(item.url.as_deref().map_or("#".to_owned(), link::outbound))
                                    target="_blank" rel="noopener noreferrer" { (title_of(item)) }
                                @if let Some(at) = item.published_at {
                                    div.meta { span { (time::item_time(at, now, &app.tz)) } }
                                }
                                (description_and_tags(item))
                            }
                        }
                    }
                }
            }
        },
    ))
}
