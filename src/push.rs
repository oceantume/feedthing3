//! Push notifications: the browsers that asked for them, and telling them
//! when a feed has something new.

use std::{env, sync::Arc, time::Duration};

use anyhow::bail;
use reqwest::{StatusCode, header};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::task::JoinSet;

use crate::{
    clock,
    feed::Feed,
    item::Item,
    link,
    webpush::{self, Subscriber, Vapid},
};

/// How long a push service holds a notification for a browser that's
/// offline. News older than a day isn't worth a notification.
const TTL_SECONDS: u32 = 24 * 60 * 60;
/// How long our VAPID token is good for; push services allow up to 24 hours.
const TOKEN_LIFETIME_SECONDS: i64 = 12 * 60 * 60;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Push {
    http: reqwest::Client,
    vapid: Arc<Vapid>,
}

#[derive(sqlx::FromRow)]
struct Subscription {
    id: i64,
    endpoint: String,
    p256dh: String,
    auth: String,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Payload {
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Push {
    /// `None` without VAPID keys: there's no bell, and nothing is sent.
    /// Reuse feedthing2's keys, and the browsers it has subscribed keep
    /// getting notifications.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let public = env::var("VAPID_PUBLIC_KEY").ok().filter(|k| !k.is_empty());
        let private = env::var("VAPID_PRIVATE_KEY").ok().filter(|k| !k.is_empty());
        let (public, private) = match (public, private) {
            (Some(public), Some(private)) => (public, private),
            (None, None) => return Ok(None),
            _ => bail!("set both VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY, or neither"),
        };
        let subject =
            env::var("VAPID_SUBJECT").unwrap_or_else(|_| "mailto:admin@localhost".to_owned());
        let vapid = Vapid::new(&public, &private, &subject)
            .map_err(|webpush::BadKey(why)| anyhow::anyhow!("VAPID keys: {why}"))?;
        Ok(Some(Self::new(crate::http_client::new(TIMEOUT), vapid)))
    }

    fn new(http: reqwest::Client, vapid: Vapid) -> Self {
        Self {
            http,
            vapid: Arc::new(vapid),
        }
    }

    /// What browsers subscribe with.
    pub fn public_key(&self) -> &str {
        &self.vapid.public_key
    }

    /// One notification per feed per poll: the item itself when there's
    /// one, a count when there are more.
    pub async fn notify_new_items(
        &self,
        db: &SqlitePool,
        feed: &Feed,
        items: &[Item],
    ) -> anyhow::Result<()> {
        self.send(db, &payload(feed, items)).await
    }

    /// To every subscribed browser at once. A browser that's gone (the push
    /// service answers 404 or 410) is forgotten; other failures are logged.
    async fn send(&self, db: &SqlitePool, payload: &Payload) -> anyhow::Result<()> {
        let subscriptions: Vec<Subscription> = sqlx::query_as("SELECT * FROM push_subscriptions")
            .fetch_all(db)
            .await?;
        if subscriptions.is_empty() {
            return Ok(());
        }
        let message = serde_json::to_vec(payload)?;
        let expires = clock::now_ms() / 1000 + TOKEN_LIFETIME_SECONDS;
        let mut sending = JoinSet::new();
        for subscription in subscriptions {
            let (push, message) = (self.clone(), message.clone());
            sending.spawn(async move {
                let result = push.send_one(&subscription, &message, expires).await;
                (subscription, result)
            });
        }
        let (mut sent, mut gone, mut failed) = (0, 0, 0);
        while let Some(joined) = sending.join_next().await {
            let Ok((subscription, result)) = joined else {
                failed += 1;
                continue;
            };
            match result {
                Ok(()) => sent += 1,
                Err(Failure::Gone) => {
                    sqlx::query("DELETE FROM push_subscriptions WHERE id = ?")
                        .bind(subscription.id)
                        .execute(db)
                        .await?;
                    gone += 1;
                }
                Err(Failure::Other(error)) => {
                    failed += 1;
                    tracing::warn!(endpoint = subscription.endpoint, error, "push failed");
                }
            }
        }
        tracing::info!("push: sent {sent}, {gone} gone and forgotten, {failed} failed");
        Ok(())
    }

    async fn send_one(
        &self,
        subscription: &Subscription,
        message: &[u8],
        expires: i64,
    ) -> Result<(), Failure> {
        let subscriber = Subscriber {
            p256dh: &subscription.p256dh,
            auth: &subscription.auth,
        };
        let body = webpush::encrypt(&subscriber, message)
            .map_err(|webpush::BadKey(why)| Failure::Other(why.to_owned()))?;
        let authorization = self
            .vapid
            .authorization(&subscription.endpoint, expires)
            .map_err(|webpush::BadKey(why)| Failure::Other(why.to_owned()))?;
        let response = self
            .http
            .post(&subscription.endpoint)
            .header(header::AUTHORIZATION, authorization)
            .header(header::CONTENT_ENCODING, "aes128gcm")
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("TTL", TTL_SECONDS)
            .body(body)
            .send()
            .await
            .map_err(|err| Failure::Other(err.to_string()))?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND | StatusCode::GONE => Err(Failure::Gone),
            status => {
                let detail = response.text().await.unwrap_or_default();
                Err(Failure::Other(format!(
                    "HTTP {}: {}",
                    status.as_u16(),
                    detail.trim()
                )))
            }
        }
    }
}

enum Failure {
    /// The browser unsubscribed, or the subscription expired.
    Gone,
    Other(String),
}

fn payload(feed: &Feed, items: &[Item]) -> Payload {
    match items {
        [item] => Payload {
            title: feed.name.clone(),
            body: item
                .title
                .clone()
                .unwrap_or_else(|| "1 new item".to_owned()),
            url: item.url.as_deref().map(link::outbound),
        },
        items => Payload {
            title: feed.name.clone(),
            body: format!("{} new items", items.len()),
            url: None,
        },
    }
}

/// Remembers a browser, or updates its keys if it's known.
pub async fn subscribe(
    db: &SqlitePool,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    now: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO push_subscriptions (endpoint, p256dh, auth, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (endpoint) DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth",
    )
    .bind(endpoint)
    .bind(p256dh)
    .bind(auth)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn unsubscribe(db: &SqlitePool, endpoint: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM push_subscriptions WHERE endpoint = ?")
        .bind(endpoint)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::{Router, http::HeaderMap, routing::post};

    use super::*;
    use crate::feed::tests::db;

    /// RFC 8291's keys: the browser's, and the sender's as our VAPID pair.
    const P256DH: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const VAPID_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const VAPID_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

    /// A push service: `/ok` takes notifications, `/gone` says the browser
    /// left. Keeps the headers it was sent.
    async fn serve() -> (String, Arc<Mutex<Vec<HeaderMap>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let app = Router::new()
            .route(
                "/ok",
                post(move |headers: HeaderMap| {
                    log.lock().unwrap().push(headers);
                    async { StatusCode::CREATED }
                }),
            )
            .route("/gone", post(|| async { StatusCode::GONE }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), seen)
    }

    fn feed() -> Feed {
        Feed {
            id: 1,
            url: "https://a.example/rss".to_owned(),
            name: "Example".to_owned(),
            poll_interval_minutes: 15,
            retention_days: None,
            fetch_full_content: false,
            enabled: true,
            next_poll_at: None,
            last_polled_at: None,
            last_error: None,
            etag: None,
            last_modified: None,
        }
    }

    fn item(title: Option<&str>, url: Option<&str>) -> Item {
        Item {
            id: 1,
            feed_id: 1,
            guid: "g".to_owned(),
            url: url.map(str::to_owned),
            title: title.map(str::to_owned),
            summary: None,
            full_content: None,
            full_content_fetched_at: None,
            full_content_error: None,
            published_at: None,
            author: None,
            ai_description: None,
            ai_tags: None,
            ai_input_tokens: None,
            ai_output_tokens: None,
        }
    }

    #[test]
    fn names_a_single_item_and_counts_several() {
        assert_eq!(
            payload(&feed(), &[item(Some("Hello"), Some("https://x.com/a"))]),
            Payload {
                title: "Example".to_owned(),
                body: "Hello".to_owned(),
                url: Some("https://xcancel.com/a".to_owned()),
            }
        );
        assert_eq!(payload(&feed(), &[item(None, None)]).body, "1 new item");
        let two = payload(&feed(), &[item(None, None), item(None, None)]);
        assert_eq!((two.body.as_str(), two.url), ("2 new items", None));
    }

    #[tokio::test]
    async fn sends_to_each_browser_and_forgets_the_ones_that_left() {
        let (db, (service, seen)) = (db().await, serve().await);
        let push = Push::new(
            crate::http_client::new(Duration::from_secs(5)),
            Vapid::new(VAPID_PUBLIC, VAPID_PRIVATE, "mailto:me@example.com").unwrap(),
        );
        subscribe(&db, &format!("{service}/ok"), P256DH, AUTH, 0)
            .await
            .unwrap();
        subscribe(&db, &format!("{service}/gone"), P256DH, AUTH, 0)
            .await
            .unwrap();

        push.notify_new_items(&db, &feed(), &[item(Some("Hi"), None)])
            .await
            .unwrap();

        let headers = seen.lock().unwrap()[0].clone();
        assert_eq!(headers["content-encoding"], "aes128gcm");
        assert_eq!(headers["ttl"], "86400");
        assert!(
            headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("vapid t=")
        );
        let left: Vec<String> = sqlx::query_scalar("SELECT endpoint FROM push_subscriptions")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(left, [format!("{service}/ok")]);
    }

    #[tokio::test]
    async fn subscribing_again_updates_the_keys() {
        let db = db().await;
        subscribe(&db, "https://push.example/1", "old", "old", 1)
            .await
            .unwrap();
        subscribe(&db, "https://push.example/1", "new", "new", 2)
            .await
            .unwrap();
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT p256dh, created_at FROM push_subscriptions")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(rows, [("new".to_owned(), 1)]);
        unsubscribe(&db, "https://push.example/1").await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM push_subscriptions")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
