//! Polling one feed: fetch it, keep what's new, and fetch the articles
//! behind new items when the feed asks for that.

use reqwest::{StatusCode, header};
use sqlx::SqlitePool;

use crate::{
    content,
    feed::{self, Feed},
    item::{self, Item},
    parse,
};

/// Feeds with years of podcast episodes run to a few MB. Anything near this
/// is something else.
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum Outcome {
    /// Not due anymore: another poll got to it first.
    Skipped,
    /// The feed answered 304: nothing changed since last time.
    Unchanged,
    /// Why the feed couldn't be read; also saved as its `last_error`.
    Failed(String),
    Polled {
        new_items: Vec<Item>,
        /// The first poll brings in a feed's whole backlog, which isn't news.
        first: bool,
    },
}

pub async fn poll(
    db: &SqlitePool,
    http: &reqwest::Client,
    feed: &Feed,
    now: i64,
) -> sqlx::Result<Outcome> {
    if !feed::claim(db, feed, now).await? {
        return Ok(Outcome::Skipped);
    }
    let fetched = match fetch(http, feed).await {
        Ok(fetched) => fetched,
        Err(error) => {
            feed::record_error(db, feed.id, &error).await?;
            return Ok(Outcome::Failed(error));
        }
    };
    let Fetched::Body {
        body,
        etag,
        last_modified,
    } = fetched
    else {
        let (etag, last_modified) = (feed.etag.as_deref(), feed.last_modified.as_deref());
        feed::record_poll(db, feed.id, now, etag, last_modified).await?;
        return Ok(Outcome::Unchanged);
    };
    let parsed = match parse::items(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            let error = error.to_string();
            feed::record_error(db, feed.id, &error).await?;
            return Ok(Outcome::Failed(error));
        }
    };
    let new_items = item::store_new(db, feed, &parsed, now).await?;
    if feed.fetch_full_content {
        for item in &new_items {
            let Some(url) = &item.url else { continue };
            let article = fetch_article(http, url).await;
            item::set_full_content(db, item.id, article.as_deref().map_err(|e| e.as_str()), now)
                .await?;
        }
    }
    feed::record_poll(db, feed.id, now, etag.as_deref(), last_modified.as_deref()).await?;
    Ok(Outcome::Polled {
        new_items,
        first: feed.last_polled_at.is_none(),
    })
}

enum Fetched {
    NotModified,
    Body {
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

async fn fetch(http: &reqwest::Client, feed: &Feed) -> Result<Fetched, String> {
    let mut request = http.get(&feed.url);
    if let Some(etag) = &feed.etag {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = &feed.last_modified {
        request = request.header(header::IF_MODIFIED_SINCE, last_modified);
    }
    let response = request.send().await.map_err(describe)?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    if !response.status().is_success() {
        return Err(format!(
            "HTTP {} fetching {}",
            response.status().as_u16(),
            feed.url
        ));
    }
    let header_text = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let etag = header_text(header::ETAG);
    let last_modified = header_text(header::LAST_MODIFIED);
    let body = read_capped(response, MAX_FEED_BYTES).await?;
    Ok(Fetched::Body {
        body,
        etag,
        last_modified,
    })
}

/// The readable part of an item's page, sanitised.
async fn fetch_article(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = http.get(url).send().await.map_err(describe)?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    // A podcast episode or a PDF has no article to read.
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !content_type.contains("html") {
        return Err(format!("not a web page ({content_type})"));
    }
    let page = read_capped(response, MAX_PAGE_BYTES).await?;
    Ok(content::extract(&page, Some(url)))
}

/// The body as text, refusing anything over `max` bytes rather than holding
/// it all in memory first.
async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<String, String> {
    let too_big = || format!("more than {} MB", max / (1024 * 1024));
    if response
        .content_length()
        .is_some_and(|len| len > max as u64)
    {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        body.extend_from_slice(&chunk);
        if body.len() > max {
            return Err(too_big());
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// reqwest's own message only says the request failed; the reason is
/// further down its chain of sources.
fn describe(err: reqwest::Error) -> String {
    if err.is_timeout() {
        return "timed out".to_owned();
    }
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        Router,
        http::{HeaderMap, StatusCode as AxumStatus, header as axum_header},
        response::IntoResponse,
        routing::get,
    };

    use super::*;
    use crate::{
        feed::tests::{db, settings},
        http_client,
    };

    const RSS: &str = r#"<rss><channel>
        <item><guid>1</guid><title>One</title><link>LINK/article</link></item>
        <item><guid>2</guid><title>Two</title></item>
    </channel></rss>"#;

    /// A local server playing the feed's site.
    async fn serve() -> String {
        let app = Router::new()
            .route(
                "/rss",
                get(|headers: HeaderMap| async move {
                    if headers
                        .get(axum_header::IF_NONE_MATCH)
                        .is_some_and(|v| v == "\"v1\"")
                    {
                        return AxumStatus::NOT_MODIFIED.into_response();
                    }
                    let host = headers[axum_header::HOST].to_str().unwrap().to_owned();
                    (
                        [(axum_header::ETAG, "\"v1\"")],
                        RSS.replace("LINK", &format!("http://{host}")),
                    )
                        .into_response()
                }),
            )
            .route(
                "/article",
                get(|| async {
                    (
                        [(axum_header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        "<body><nav>Menu</nav><article><p>The article.</p></article></body>",
                    )
                }),
            )
            .route(
                "/broken",
                get(|| async { AxumStatus::INTERNAL_SERVER_ERROR }),
            )
            .route(
                "/not-a-feed",
                get(|| async { "<html><body>Hi</body></html>" }),
            )
            .route(
                "/hangs",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    RSS
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    async fn feed_at(db: &SqlitePool, url: &str, full_content: bool) -> Feed {
        let mut settings = settings(url);
        settings.fetch_full_content = full_content;
        let id = feed::create(db, &settings, 0).await.unwrap().unwrap();
        feed::get(db, id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn keeps_new_items_and_says_whether_it_was_the_first_poll() {
        let (db, site) = (db().await, serve().await);
        let http = http_client::new(Duration::from_secs(5));
        let feed = feed_at(&db, &format!("{site}/rss"), false).await;

        let Outcome::Polled { new_items, first } = poll(&db, &http, &feed, 1000).await.unwrap()
        else {
            panic!("expected a poll");
        };
        assert!(first);
        assert_eq!(new_items.len(), 2);

        let feed = feed::get(&db, feed.id).await.unwrap().unwrap();
        assert_eq!(feed.last_polled_at, Some(1000));
        assert_eq!(feed.next_poll_at, Some(1000 + 15 * 60_000));
        assert_eq!(feed.etag.as_deref(), Some("\"v1\""));
    }

    #[tokio::test]
    async fn polls_only_when_due() {
        let (db, site) = (db().await, serve().await);
        let http = http_client::new(Duration::from_secs(5));
        let feed = feed_at(&db, &format!("{site}/rss"), false).await;
        poll(&db, &http, &feed, 1000).await.unwrap();

        // A tick that read the feed before the first poll booked the next one.
        let outcome = poll(&db, &http, &feed, 2000).await.unwrap();
        assert!(matches!(outcome, Outcome::Skipped), "{outcome:?}");
    }

    #[tokio::test]
    async fn sends_back_the_etag_and_takes_a_304_as_nothing_new() {
        let (db, site) = (db().await, serve().await);
        let http = http_client::new(Duration::from_secs(5));
        let feed = feed_at(&db, &format!("{site}/rss"), false).await;
        poll(&db, &http, &feed, 1000).await.unwrap();

        let feed = feed::get(&db, feed.id).await.unwrap().unwrap();
        let later = feed.next_poll_at.unwrap();
        let outcome = poll(&db, &http, &feed, later).await.unwrap();
        assert!(matches!(outcome, Outcome::Unchanged), "{outcome:?}");
        let feed = feed::get(&db, feed.id).await.unwrap().unwrap();
        assert_eq!(feed.last_polled_at, Some(later));
        assert_eq!(feed.etag.as_deref(), Some("\"v1\""), "keeps the etag");
    }

    #[tokio::test]
    async fn fetches_the_article_behind_each_new_item_when_asked() {
        let (db, site) = (db().await, serve().await);
        let http = http_client::new(Duration::from_secs(5));
        let feed = feed_at(&db, &format!("{site}/rss"), true).await;
        poll(&db, &http, &feed, 1000).await.unwrap();

        let item: Item = sqlx::query_as("SELECT * FROM items WHERE guid = '1'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(item.full_content.as_deref(), Some("<p>The article.</p>"));
        assert_eq!(item.full_content_fetched_at, Some(1000));
        let without_link: Item = sqlx::query_as("SELECT * FROM items WHERE guid = '2'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(without_link.full_content_fetched_at, None);
    }

    #[tokio::test]
    async fn records_why_a_poll_failed_and_keeps_the_last_good_time() {
        let (db, site) = (db().await, serve().await);
        let http = http_client::new(Duration::from_secs(5));
        for (path, expected) in [
            ("/broken", "HTTP 500 fetching"),
            ("/not-a-feed", "not an RSS, Atom or JSON feed"),
        ] {
            let feed = feed_at(&db, &format!("{site}{path}"), false).await;
            let outcome = poll(&db, &http, &feed, 1000).await.unwrap();
            assert!(
                matches!(&outcome, Outcome::Failed(error) if error.contains(expected)),
                "{outcome:?}"
            );
            let feed = feed::get(&db, feed.id).await.unwrap().unwrap();
            assert!(feed.last_error.unwrap().contains(expected));
            assert_eq!(feed.last_polled_at, None);
        }
    }

    #[tokio::test]
    async fn gives_up_on_a_feed_that_never_answers() {
        let (db, site) = (db().await, serve().await);
        let http = http_client::new(Duration::from_millis(200));
        let feed = feed_at(&db, &format!("{site}/hangs"), false).await;

        let outcome = tokio::time::timeout(Duration::from_secs(5), poll(&db, &http, &feed, 1000))
            .await
            .expect("the client's timeout should end the poll")
            .unwrap();
        assert!(
            matches!(&outcome, Outcome::Failed(error) if error == "timed out"),
            "{outcome:?}"
        );
    }
}
