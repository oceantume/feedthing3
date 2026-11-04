//! Static files, built into the binary and linked with content-hashed URLs
//! so browsers can cache them for good: when a file changes, its URL changes.

use axum::{
    Router,
    body::Bytes,
    extract::{Path, RawQuery},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

const DIR: &str = "static";

struct Asset {
    name: &'static str,
    bytes: &'static [u8],
    /// A hash of `bytes`: the `v` in its URL, and its ETag.
    version: u64,
}

/// A file in `static/`, hashed while compiling.
macro_rules! asset {
    ($name:literal) => {{
        const BYTES: &[u8] = include_bytes!(concat!("../../static/", $name));
        Asset {
            name: $name,
            bytes: BYTES,
            version: fnv1a(BYTES),
        }
    }};
}

/// Every file in `static/`: a file added there needs a line here.
static ASSETS: &[Asset] = &[
    asset!("app.css"),
    asset!("app.mjs"),
    asset!("favicon.svg"),
    asset!("manifest.json"),
    asset!("sw.js"),
];

/// FNV-1a: tiny, and a `const fn`, so the compiler runs it. Only needs to
/// change when the contents do.
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u64).wrapping_mul(0x100000001b3);
        i += 1;
    }
    hash
}

/// Looking files up by name in here, rather than reading a requested path
/// from disk, keeps `../` out.
fn get_asset(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.name == name)
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
    format!("/{DIR}/{name}?v={:016x}", asset.version)
}

/// Serves `static/`.
pub fn service() -> Router {
    Router::new().route("/{name}", get(file))
}

/// `GET /static/{name}`
///
/// Versioned URLs never change content, so they're cached for a year without
/// revalidation. Anything else is revalidated on every use.
async fn file(Path(name): Path<String>, RawQuery(query): RawQuery, headers: HeaderMap) -> Response {
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
    let etag = format!("\"{:016x}\"", asset.version);
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
        [(header::CONTENT_TYPE, content_type(name))],
        Bytes::from_static(asset.bytes),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn has_every_file_in_static() {
        for entry in std::fs::read_dir(DIR).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                get_asset(&name).is_some(),
                "static/{name} needs a line in ASSETS"
            );
        }
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
