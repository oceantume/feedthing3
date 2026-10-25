//! Static files, linked with content-hashed URLs so browsers can cache them
//! for good: when a file changes, its URL changes.

use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hasher},
    path::Path,
    sync::{LazyLock, Mutex},
};

use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
};
use tower_http::services::ServeDir;

const DIR: &str = "static";

/// `/static/{name}?v={hash of its contents}`.
///
/// Debug builds re-hash on every call, so an edited file shows up on refresh
/// without a restart. Release builds hash each file once.
pub fn url(name: &str) -> String {
    let version = if cfg!(debug_assertions) {
        version_of(Path::new(DIR), name)
    } else {
        static VERSIONS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
        VERSIONS
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_insert_with(|| version_of(Path::new(DIR), name))
            .clone()
    };
    format!("/{DIR}/{name}?v={version}")
}

fn version_of(dir: &Path, name: &str) -> String {
    match fs::read(dir.join(name)) {
        Ok(bytes) => {
            // Only needs to change when the contents do; stability across
            // Rust versions doesn't matter for cache busting.
            let mut hasher = DefaultHasher::new();
            hasher.write(&bytes);
            format!("{:016x}", hasher.finish())
        }
        Err(err) => {
            tracing::warn!(%err, name, "static file missing");
            "missing".to_owned()
        }
    }
}

/// Serves `static/` with cache headers.
pub fn service() -> Router {
    Router::new()
        .fallback_service(ServeDir::new(DIR))
        .layer(middleware::from_fn(cache_headers))
}

/// Versioned URLs never change content, so they're cached for a year without
/// revalidation. Anything else is revalidated on every use.
async fn cache_headers(request: Request, next: Next) -> Response {
    let versioned = request
        .uri()
        .query()
        .is_some_and(|q| q.split('&').any(|pair| pair.starts_with("v=")));
    let mut response = next.run(request).await;
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        let value = if versioned {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::version_of;

    #[test]
    fn version_changes_with_contents() {
        let dir = std::env::temp_dir().join(format!("feedthing-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.css");

        std::fs::write(&file, "body { color: red }").unwrap();
        let first = version_of(&dir, "a.css");
        assert_eq!(
            version_of(&dir, "a.css"),
            first,
            "stable for the same contents"
        );

        std::fs::write(&file, "body { color: blue }").unwrap();
        assert_ne!(
            version_of(&dir, "a.css"),
            first,
            "changes with the contents"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
