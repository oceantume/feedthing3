//! CSRF guard. Form posts are "simple" cross-origin requests (no CORS
//! preflight), so any site could submit one to us; reject state-changing
//! requests whose Origin isn't this host. Browsers send Origin on every POST.

use axum::{
    extract::Request,
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub async fn same_origin_writes(request: Request, next: Next) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        let headers = request.headers();
        let origin_host = headers
            .get(header::ORIGIN)
            .and_then(|o| o.to_str().ok())
            .and_then(|o| o.split_once("://").map(|(_, host)| host));
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
        if origin_host.is_none() || origin_host != host {
            return (StatusCode::FORBIDDEN, "Cross-origin request rejected").into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::post,
    };
    use tower::ServiceExt;

    async fn status(method: &str, origin: Option<&str>) -> StatusCode {
        let app = Router::new()
            .route("/", post(|| async {}).get(|| async {}))
            .layer(middleware::from_fn(super::same_origin_writes));
        let mut request = Request::builder()
            .method(method)
            .uri("/")
            .header("host", "feedthing.example");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = app
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        response.status()
    }

    #[tokio::test]
    async fn allows_same_origin_posts() {
        assert_eq!(
            status("POST", Some("https://feedthing.example")).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn rejects_cross_origin_and_originless_posts() {
        assert_eq!(
            status("POST", Some("https://evil.example")).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status("POST", Some("https://feedthing.example.evil.example")).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(status("POST", None).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn allows_reads_from_anywhere() {
        assert_eq!(
            status("GET", Some("https://evil.example")).await,
            StatusCode::OK
        );
        assert_eq!(status("GET", None).await, StatusCode::OK);
    }
}
