//! The home page: what came in since yesterday.

use axum::{Router, routing::get};
use maud::{Markup, html};

use super::{AppState, layout};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(show))
}

/// `GET /`
async fn show() -> Markup {
    layout::page(
        "Feedthing",
        html! {
            h1 { "Recent items" }
            p.muted { "No recent items." }
        },
    )
}
