//! What the notification bell talks to, and the service worker it
//! registers. JSON, because that's what the browser's push API hands over.

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

use super::{AppState, error::AppError};
use crate::{clock, push};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sw.js", get(service_worker))
        .route("/push/vapid-public-key", get(public_key))
        .route("/push/subscribe", post(subscribe).delete(unsubscribe))
}

/// `GET /sw.js`
///
/// From the root, so it controls the whole app. Revalidated on every load:
/// browsers check it for updates themselves.
async fn service_worker() -> Result<Response, AppError> {
    let script = tokio::fs::read("static/sw.js").await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        script,
    )
        .into_response())
}

/// `GET /push/vapid-public-key`
///
/// 503 when push isn't set up, which tells the bell to stay hidden.
async fn public_key(State(app): State<AppState>) -> Response {
    match &app.push {
        Some(push) => Json(json!({ "publicKey": push.public_key() })).into_response(),
        None => not_configured(),
    }
}

/// A browser's `PushSubscription.toJSON()`, as the bell sends it.
#[derive(Deserialize)]
struct Subscription {
    endpoint: String,
    keys: Keys,
}

#[derive(Deserialize)]
struct Keys {
    p256dh: String,
    auth: String,
}

/// `POST /push/subscribe`
async fn subscribe(State(app): State<AppState>, body: String) -> Result<Response, AppError> {
    if app.push.is_none() {
        return Ok(not_configured());
    }
    let Ok(subscription) = serde_json::from_str::<Subscription>(&body) else {
        return Ok(bad_request("Invalid subscription"));
    };
    let Subscription { endpoint, keys } = subscription;
    if !endpoint.starts_with("https://") || keys.p256dh.is_empty() || keys.auth.is_empty() {
        return Ok(bad_request("Invalid subscription"));
    }
    push::subscribe(
        &app.db,
        &endpoint,
        &keys.p256dh,
        &keys.auth,
        clock::now_ms(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct Unsubscribe {
    endpoint: String,
}

/// `DELETE /push/subscribe`
async fn unsubscribe(State(app): State<AppState>, body: String) -> Result<Response, AppError> {
    let Ok(Unsubscribe { endpoint }) = serde_json::from_str(&body) else {
        return Ok(bad_request("endpoint required"));
    };
    push::unsubscribe(&app.db, &endpoint).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "Push notifications not configured" })),
    )
        .into_response()
}

fn bad_request(error: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}
