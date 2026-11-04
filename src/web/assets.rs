//! Static files, held in memory and linked with content-hashed URLs so
//! browsers can cache them for good: when a file changes, its URL changes.

use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hasher},
    io,
    path::Path,
    sync::OnceLock,
};

use axum::{
    Router,
    body::Bytes,
    extract::{Path as UrlPath, RawQuery},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

const DIR: &str = "static";

#[derive(Clone)]
struct Asset {
    bytes: Bytes,
    /// A hash of `bytes`: the `v` in its URL, and its ETag.
    version: String,
    content_type: &'static str,
}

/// Every file in `static/`, read by `load`. Release builds only.
static ASSETS: OnceLock<HashMap<String, Asset>> = OnceLock::new();

/// Reads `static/`, before the app serves anything: no request waits on the
/// disk, and a release missing its files doesn't start. Debug builds read it
/// again on every use, so an edited file shows up on refresh without a
/// restart.
pub fn load() -> io::Result<()> {
    let assets = read_all(Path::new(DIR))?;
    if !cfg!(debug_assertions) {
        ASSETS.set(assets).ok();
    }
    Ok(())
}

/// Borrowed from what `load` read, or, in debug builds, freshly read.
fn get_asset(name: &str) -> Option<Cow<'static, Asset>> {
    if cfg!(debug_assertions) {
        read_all(Path::new(DIR)).ok()?.remove(name).map(Cow::Owned)
    } else {
        let assets = ASSETS.get().expect("assets::load runs at startup");
        assets.get(name).map(Cow::Borrowed)
    }
}

/// Every file in `dir`, by name. Looking files up by name in here, rather
/// than joining a requested path onto `dir`, keeps `../` out.
fn read_all(dir: &Path) -> io::Result<HashMap<String, Asset>> {
    let mut assets = HashMap::new();
    for entry in fs::read_dir(dir)?.flatten() {
        // Skips folders, which `fs::read` refuses.
        let (Ok(name), Ok(bytes)) = (entry.file_name().into_string(), fs::read(entry.path()))
        else {
            continue;
        };
        let asset = Asset {
            version: version_of(&bytes),
            content_type: content_type(&name),
            bytes: bytes.into(),
        };
        assets.insert(name, asset);
    }
    Ok(assets)
}

fn version_of(bytes: &[u8]) -> String {
    // Only needs to change when the contents do; stability across Rust
    // versions doesn't matter for cache busting.
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("{:016x}", hasher.finish())
}

/// The types of what's in `static/`.
fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// `/static/{name}?v={hash of its contents}`.
pub fn url(name: &str) -> String {
    let Some(asset) = get_asset(name) else {
        tracing::warn!(name, "static file missing");
        return format!("/{DIR}/{name}?v=missing");
    };
    format!("/{DIR}/{name}?v={}", asset.version)
}

/// Serves `static/`.
pub fn service() -> Router {
    Router::new().route("/{name}", get(file))
}

/// `GET /static/{name}`
///
/// Versioned URLs never change content, so they're cached for a year without
/// revalidation. Anything else is revalidated on every use.
async fn file(
    UrlPath(name): UrlPath<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let versioned = query.is_some_and(|q| q.split('&').any(|pair| pair.starts_with("v=")));
    let cache_control = if versioned {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    respond(&name, &headers, cache_control)
}

/// The file `name` from `static/`, or a 404: a 304 when the browser's copy,
/// named in `request`'s `If-None-Match`, is current.
pub fn respond(name: &str, request: &HeaderMap, cache_control: &'static str) -> Response {
    let Some(asset) = get_asset(name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", asset.version);
    let current = request
        .get(header::IF_NONE_MATCH)
        .and_then(|tags| tags.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .map(str::trim)
                .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
        });
    let headers = [
        (header::ETAG, etag),
        (header::CACHE_CONTROL, cache_control.to_owned()),
    ];
    if current {
        return (StatusCode::NOT_MODIFIED, headers).into_response();
    }
    (
        headers,
        [(header::CONTENT_TYPE, asset.content_type)],
        // A handle on the same bytes, not a copy.
        asset.bytes.clone(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn version_changes_with_contents() {
        let dir = std::env::temp_dir().join(format!("feedthing-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.css");
        let version = || read_all(&dir).unwrap().remove("a.css").unwrap().version;

        std::fs::write(&file, "body { color: red }").unwrap();
        let first = version();
        assert_eq!(version(), first, "stable for the same contents");

        std::fs::write(&file, "body { color: blue }").unwrap();
        assert_ne!(version(), first, "changes with the contents");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    async fn get(uri: &str, if_none_match: Option<&str>) -> Response {
        let mut request = Request::get(uri);
        if let Some(etag) = if_none_match {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        service()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn header_of(response: &Response, name: header::HeaderName) -> &str {
        response.headers()[name].to_str().unwrap()
    }

    #[tokio::test]
    async fn serves_files_cached_for_good_when_versioned() {
        let url = url("app.css");
        let response = get(url.trim_start_matches("/static"), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_of(&response, header::CONTENT_TYPE),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            header_of(&response, header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        let version = url.split_once("?v=").unwrap().1;
        assert_eq!(header_of(&response, header::ETAG), format!("\"{version}\""));
    }

    #[tokio::test]
    async fn answers_304_to_a_current_copy() {
        let etag = header_of(&get("/favicon.svg", None).await, header::ETAG).to_owned();
        let response = get("/favicon.svg", Some(&etag)).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(header_of(&response, header::CACHE_CONTROL), "no-cache");

        let stale = get("/favicon.svg", Some("\"0000000000000000\"")).await;
        assert_eq!(stale.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn only_serves_what_is_in_static() {
        for uri in ["/nope.css", "/..%2FCargo.toml", "/%2E%2E%2FCargo.toml"] {
            assert_eq!(
                get(uri, None).await.status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }
}
