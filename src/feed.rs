//! Feeds: what to poll, how often, and how long to keep what comes in.

use sqlx::SqlitePool;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Feed {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub poll_interval_minutes: i64,
    pub retention_days: Option<i64>,
    pub fetch_full_content: bool,
    pub enabled: bool,
    pub next_poll_at: Option<i64>,
    pub last_polled_at: Option<i64>,
    pub last_error: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub created_at: i64,
}

/// What a person sets on a feed.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub name: String,
    pub url: String,
    pub poll_interval_minutes: i64,
    pub retention_days: Option<i64>,
    pub fetch_full_content: bool,
    pub enabled: bool,
}

#[derive(sqlx::FromRow)]
pub struct WithCount {
    #[sqlx(flatten)]
    pub feed: Feed,
    pub item_count: i64,
}

/// Saving would give two feeds the same URL.
#[derive(Debug)]
pub struct DuplicateUrl;

/// Newest first, each with how many items it has.
pub async fn list_with_counts(db: &SqlitePool) -> sqlx::Result<Vec<WithCount>> {
    sqlx::query_as(
        "SELECT feeds.*, (SELECT count(*) FROM items WHERE items.feed_id = feeds.id) AS item_count
         FROM feeds ORDER BY created_at DESC, id DESC",
    )
    .fetch_all(db)
    .await
}

pub async fn get(db: &SqlitePool, id: i64) -> sqlx::Result<Option<Feed>> {
    sqlx::query_as("SELECT * FROM feeds WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn create(
    db: &SqlitePool,
    settings: &Settings,
    now: i64,
) -> anyhow::Result<Result<i64, DuplicateUrl>> {
    let inserted = sqlx::query_scalar(
        "INSERT INTO feeds (name, url, poll_interval_minutes, retention_days, fetch_full_content, enabled, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&settings.name)
    .bind(&settings.url)
    .bind(settings.poll_interval_minutes)
    .bind(settings.retention_days)
    .bind(settings.fetch_full_content)
    .bind(settings.enabled)
    .bind(now)
    .fetch_one(db)
    .await;
    duplicate_url(inserted)
}

/// A new URL is a different feed as far as caching goes, so what the old one
/// sent for conditional requests is dropped.
pub async fn update(
    db: &SqlitePool,
    id: i64,
    settings: &Settings,
) -> anyhow::Result<Result<(), DuplicateUrl>> {
    let updated = sqlx::query(
        "UPDATE feeds SET
           etag = CASE WHEN url = ?1 THEN etag END,
           last_modified = CASE WHEN url = ?1 THEN last_modified END,
           url = ?1, name = ?2, poll_interval_minutes = ?3, retention_days = ?4,
           fetch_full_content = ?5, enabled = ?6
         WHERE id = ?7",
    )
    .bind(&settings.url)
    .bind(&settings.name)
    .bind(settings.poll_interval_minutes)
    .bind(settings.retention_days)
    .bind(settings.fetch_full_content)
    .bind(settings.enabled)
    .bind(id)
    .execute(db)
    .await
    .map(|_| ());
    duplicate_url(updated)
}

/// Enabled feeds whose next poll is due, or that were never polled.
pub async fn due(db: &SqlitePool, now: i64) -> sqlx::Result<Vec<Feed>> {
    sqlx::query_as(
        "SELECT * FROM feeds WHERE enabled AND (next_poll_at IS NULL OR next_poll_at <= ?)
         ORDER BY id",
    )
    .bind(now)
    .fetch_all(db)
    .await
}

/// Books the feed's next poll, if it's still due. False when something else
/// got to it first, so a feed is never polled twice at once. The next poll is
/// set before this one runs: a feed that keeps failing is retried on its
/// usual schedule, not in a tight loop.
pub async fn claim(db: &SqlitePool, feed: &Feed, now: i64) -> sqlx::Result<bool> {
    let claimed = sqlx::query(
        "UPDATE feeds SET next_poll_at = ? + poll_interval_minutes * 60000
         WHERE id = ? AND (next_poll_at IS NULL OR next_poll_at <= ?)",
    )
    .bind(now)
    .bind(feed.id)
    .bind(now)
    .execute(db)
    .await?;
    Ok(claimed.rows_affected() == 1)
}

/// A poll that reached the feed. The validators are what to send next time
/// (`None` forgets them); a 304 passes the ones it was asked with.
pub async fn record_poll(
    db: &SqlitePool,
    id: i64,
    now: i64,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE feeds SET last_polled_at = ?, last_error = NULL, etag = ?, last_modified = ?
         WHERE id = ?",
    )
    .bind(now)
    .bind(etag)
    .bind(last_modified)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// A poll that failed. `last_polled_at` stays at the last one that worked.
pub async fn record_error(db: &SqlitePool, id: i64, error: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE feeds SET last_error = ? WHERE id = ?")
        .bind(error)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Its items go with it.
pub async fn delete(db: &SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM feeds WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

fn duplicate_url<T>(result: sqlx::Result<T>) -> anyhow::Result<Result<T, DuplicateUrl>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => Ok(Err(DuplicateUrl)),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub async fn db() -> SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&db)
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        db
    }

    pub fn settings(url: &str) -> Settings {
        Settings {
            name: "Example".to_owned(),
            url: url.to_owned(),
            poll_interval_minutes: 15,
            retention_days: None,
            fetch_full_content: false,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn turns_away_a_second_feed_with_the_same_url() {
        let db = db().await;
        create(&db, &settings("https://a.example/rss"), 1)
            .await
            .unwrap()
            .unwrap();
        let again = create(&db, &settings("https://a.example/rss"), 2)
            .await
            .unwrap();
        assert!(again.is_err());
    }

    #[tokio::test]
    async fn moving_a_feed_to_a_taken_url_is_turned_away() {
        let db = db().await;
        create(&db, &settings("https://a.example/rss"), 1)
            .await
            .unwrap()
            .unwrap();
        let b = create(&db, &settings("https://b.example/rss"), 1)
            .await
            .unwrap()
            .unwrap();
        let moved = update(&db, b, &settings("https://a.example/rss"))
            .await
            .unwrap();
        assert!(moved.is_err());
    }

    #[tokio::test]
    async fn a_new_url_forgets_the_old_ones_cache_validators() {
        let db = db().await;
        let id = create(&db, &settings("https://a.example/rss"), 1)
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE feeds SET etag = 'x', last_modified = 'y'")
            .execute(&db)
            .await
            .unwrap();

        let mut renamed = settings("https://a.example/rss");
        renamed.name = "Renamed".to_owned();
        update(&db, id, &renamed).await.unwrap().unwrap();
        let feed = get(&db, id).await.unwrap().unwrap();
        assert_eq!(feed.name, "Renamed");
        assert_eq!(feed.etag.as_deref(), Some("x"), "same URL keeps them");

        update(&db, id, &settings("https://b.example/rss"))
            .await
            .unwrap()
            .unwrap();
        let feed = get(&db, id).await.unwrap().unwrap();
        assert_eq!(feed.url, "https://b.example/rss");
        assert_eq!(feed.etag, None);
        assert_eq!(feed.last_modified, None);
    }
}
