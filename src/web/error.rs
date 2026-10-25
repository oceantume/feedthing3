//! What goes wrong, and the page people see when it does.

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use maud::{Markup, html};

use super::layout;

#[derive(Debug)]
pub enum AppError {
    NotFound,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self::Internal(err.into())
    }
}

/// A status and a word for it; [`page`] gives it a page on the way out.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound => (StatusCode::NOT_FOUND, "Not found").into_response(),
            Self::Internal(err) => {
                tracing::error!(error = ?err, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response()
            }
        }
    }
}

/// Gives error responses that have no page of their own a page: an
/// [`AppError`], a route that doesn't exist, a form that can't be read, the
/// CSRF guard, a panic.
///
/// Left alone: responses that are already HTML (a form shown again with its
/// error), and JSON answers to the notification bell.
pub async fn page(request: Request, next: Next) -> Response {
    // Only a GET is safe to send again from "Try again".
    let retry = (request.method() == Method::GET).then(|| request.uri().to_string());

    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) || has_body_type(&response) {
        return response;
    }

    // Keeps the status and headers like `Allow`; only the body changes.
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // A 404 is cacheable by default, and the page may exist a minute from now.
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let page = view(status, retry.as_deref());
    Response::from_parts(parts, Body::from(page.into_string()))
}

/// HTML and JSON were written for whoever asked; plain text is only the
/// short word [`AppError`] and axum's rejections answer with.
fn has_body_type(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html") || v.starts_with("application/json"))
}

fn view(status: StatusCode, retry: Option<&str>) -> Markup {
    let (title, explanation) = match status {
        StatusCode::NOT_FOUND => (
            "Not found",
            "There's nothing here. It may have been deleted.",
        ),
        StatusCode::FORBIDDEN => (
            "Not allowed",
            "This request came from another site, so it was turned away.",
        ),
        s if s.is_client_error() => (
            "Something's off with that request",
            "The app couldn't read what was sent.",
        ),
        _ => (
            "Something went wrong",
            "It's been logged. Trying again may work.",
        ),
    };
    layout::page(
        title,
        html! {
            section.card {
                h1 { (title) }
                p.muted { (explanation) }
                p {
                    @if let Some(retry) = retry {
                        a href=(retry) { "Try again" } " · "
                    }
                    a href="/" { "Back to the summary" }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, http::Request, middleware, routing::get};
    use tower::ServiceExt;

    use super::*;

    async fn through_page(app: Router) -> Response {
        app.layer(middleware::from_fn(page))
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn content_type(response: &Response) -> &str {
        response.headers()[header::CONTENT_TYPE].to_str().unwrap()
    }

    #[tokio::test]
    async fn gives_bare_errors_a_page_and_keeps_their_status() {
        let app = Router::new().route("/", get(|| async { AppError::NotFound }));
        let response = through_page(app).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(content_type(&response).starts_with("text/html"));
    }

    #[tokio::test]
    async fn leaves_json_errors_alone() {
        let app = Router::new().route(
            "/",
            get(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({ "error": "nope" })),
                )
            }),
        );
        let response = through_page(app).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(content_type(&response), "application/json");
    }
}
