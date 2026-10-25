//! Items: what feeds brought in, kept until their feed's retention runs out.

use sqlx::SqlitePool;

use crate::{clock::DAY_MS, feed::Feed, parse};

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Item {
    pub id: i64,
    pub feed_id: i64,
    pub guid: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub full_content: Option<String>,
    pub full_content_fetched_at: Option<i64>,
    pub full_content_error: Option<String>,
    pub published_at: Option<i64>,
    pub author: Option<String>,
    pub ai_description: Option<String>,
    /// A JSON array of strings; see [`Item::tags`].
    pub ai_tags: Option<String>,
    pub ai_input_tokens: Option<i64>,
    pub ai_output_tokens: Option<i64>,
}

impl Item {
    pub fn tags(&self) -> Vec<String> {
        self.ai_tags
            .as_deref()
            .and_then(|tags| serde_json::from_str(tags).ok())
            .unwrap_or_default()
    }
}

/// An item with the name of its feed, for lists that show both.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Listed {
    #[sqlx(flatten)]
    pub item: Item,
    pub feed_name: String,
}

pub const MAX_PAGE: i64 = 100;
pub const DEFAULT_PAGE: i64 = 50;

/// Which items to list, newest first.
#[derive(Debug, Default)]
pub struct Filter<'a> {
    pub feed_id: Option<i64>,
    /// Matched against titles, ignoring case.
    pub search: Option<&'a str>,
    /// Published at or after, epoch milliseconds.
    pub since: Option<i64>,
    /// Published at or before.
    pub until: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

pub async fn list(db: &SqlitePool, filter: &Filter<'_>) -> sqlx::Result<Vec<Listed>> {
    // `%` and `_` in a search mean themselves.
    let pattern = filter.search.map(|search| {
        let escaped = search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    });
    sqlx::query_as(
        "SELECT items.*, feeds.name AS feed_name FROM items JOIN feeds ON feeds.id = items.feed_id
         WHERE (?1 IS NULL OR items.feed_id = ?1)
           AND (?2 IS NULL OR items.title LIKE ?2 ESCAPE '\\')
           AND (?3 IS NULL OR items.published_at >= ?3)
           AND (?4 IS NULL OR items.published_at <= ?4)
         ORDER BY items.published_at DESC, items.id DESC
         LIMIT ?5 OFFSET ?6",
    )
    .bind(filter.feed_id)
    .bind(pattern)
    .bind(filter.since)
    .bind(filter.until)
    .bind(filter.limit.clamp(1, MAX_PAGE))
    .bind(filter.offset.max(0))
    .fetch_all(db)
    .await
}

pub async fn get(db: &SqlitePool, id: i64) -> sqlx::Result<Option<Listed>> {
    sqlx::query_as(
        "SELECT items.*, feeds.name AS feed_name FROM items JOIN feeds ON feeds.id = items.feed_id
         WHERE items.id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Items published between `since` and `until`, grouped by feed: feeds by
/// name, items newest first. At most 1000 in all.
pub async fn grouped_by_feed(
    db: &SqlitePool,
    since: i64,
    until: i64,
) -> sqlx::Result<Vec<(String, Vec<Listed>)>> {
    let items: Vec<Listed> = sqlx::query_as(
        "SELECT items.*, feeds.name AS feed_name FROM items JOIN feeds ON feeds.id = items.feed_id
         WHERE items.published_at BETWEEN ? AND ?
         ORDER BY feeds.name COLLATE NOCASE, feeds.id, items.published_at DESC, items.id DESC
         LIMIT 1000",
    )
    .bind(since)
    .bind(until)
    .fetch_all(db)
    .await?;
    let mut groups: Vec<(i64, String, Vec<Listed>)> = Vec::new();
    for listed in items {
        match groups.last_mut() {
            Some((feed_id, _, group)) if *feed_id == listed.item.feed_id => group.push(listed),
            _ => groups.push((listed.item.feed_id, listed.feed_name.clone(), vec![listed])),
        }
    }
    Ok(groups
        .into_iter()
        .map(|(_, name, items)| (name, items))
        .collect())
}

/// Saves the items the feed doesn't have yet and returns them, oldest
/// first as the feed lists them. An item is known by its guid, or its URL
/// when it has none; one with neither can't be told apart from the next, so
/// it's skipped. Items already older than the feed's retention are too.
pub async fn store_new(
    db: &SqlitePool,
    feed: &Feed,
    items: &[parse::Item],
    now: i64,
) -> sqlx::Result<Vec<Item>> {
    let cutoff = feed.retention_days.map(|days| now - days * DAY_MS);
    let mut tx = db.begin().await?;
    let mut stored = Vec::new();
    for item in items {
        let Some(guid) = item.guid.as_ref().or(item.url.as_ref()) else {
            continue;
        };
        if cutoff.is_some_and(|cutoff| item.published_at.is_some_and(|at| at < cutoff)) {
            continue;
        }
        let inserted: Option<Item> = sqlx::query_as(
            "INSERT INTO items (feed_id, guid, url, title, summary, author, published_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (feed_id, guid) DO NOTHING
             RETURNING *",
        )
        .bind(feed.id)
        .bind(guid)
        .bind(&item.url)
        .bind(&item.title)
        .bind(&item.summary)
        .bind(&item.author)
        .bind(item.published_at)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        stored.extend(inserted);
    }
    tx.commit().await?;
    Ok(stored)
}

/// The article behind an item, or why it couldn't be had.
pub async fn set_full_content(
    db: &SqlitePool,
    id: i64,
    content: Result<&str, &str>,
    now: i64,
) -> sqlx::Result<()> {
    let (content, error) = match content {
        Ok(content) => (Some(content), None),
        Err(error) => (None, Some(error)),
    };
    sqlx::query(
        "UPDATE items SET full_content = ?, full_content_error = ?, full_content_fetched_at = ?
         WHERE id = ?",
    )
    .bind(content)
    .bind(error)
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Deletes items that came in longer ago than their feed keeps them.
/// Counts from when an item came in, not when it was published, as
/// feedthing2 did. Returns how many went.
pub async fn purge_expired(db: &SqlitePool, now: i64) -> sqlx::Result<u64> {
    let deleted = sqlx::query(
        "DELETE FROM items WHERE created_at <
           ? - (SELECT retention_days FROM feeds WHERE feeds.id = items.feed_id) * ?",
    )
    .bind(now)
    .bind(DAY_MS)
    .execute(db)
    .await?;
    Ok(deleted.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{self, tests::settings};

    fn parsed(guid: Option<&str>, url: Option<&str>, published_at: Option<i64>) -> parse::Item {
        parse::Item {
            guid: guid.map(str::to_owned),
            url: url.map(str::to_owned),
            title: Some("A title".to_owned()),
            published_at,
            ..parse::Item::default()
        }
    }

    async fn feed_with(db: &SqlitePool, retention_days: Option<i64>) -> Feed {
        let mut settings = settings("https://a.example/rss");
        settings.retention_days = retention_days;
        let id = feed::create(db, &settings, 0).await.unwrap().unwrap();
        feed::get(db, id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn stores_each_item_once_known_by_guid_or_url() {
        let db = feed::tests::db().await;
        let feed = feed_with(&db, None).await;
        let items = [
            parsed(Some("g1"), Some("https://a.example/1"), None),
            parsed(None, Some("https://a.example/2"), None),
            parsed(None, None, None),
        ];
        let stored = store_new(&db, &feed, &items, 1).await.unwrap();
        assert_eq!(
            stored.iter().map(|i| i.guid.as_str()).collect::<Vec<_>>(),
            ["g1", "https://a.example/2"]
        );

        let again = store_new(&db, &feed, &items, 2).await.unwrap();
        assert!(again.is_empty());
    }

    #[tokio::test]
    async fn skips_items_already_past_retention_but_keeps_undated_ones() {
        let db = feed::tests::db().await;
        let feed = feed_with(&db, Some(7)).await;
        let now = 100 * DAY_MS;
        let items = [
            parsed(Some("old"), None, Some(now - 8 * DAY_MS)),
            parsed(Some("recent"), None, Some(now - 6 * DAY_MS)),
            parsed(Some("undated"), None, None),
        ];
        let stored = store_new(&db, &feed, &items, now).await.unwrap();
        assert_eq!(
            stored.iter().map(|i| i.guid.as_str()).collect::<Vec<_>>(),
            ["recent", "undated"]
        );
    }

    #[tokio::test]
    async fn lists_newest_first_with_filters_and_a_literal_search() {
        let db = feed::tests::db().await;
        let feed = feed_with(&db, None).await;
        let mut items = vec![
            parsed(Some("a"), None, Some(1000)),
            parsed(Some("b"), None, Some(3000)),
            parsed(Some("c"), None, None),
        ];
        items[0].title = Some("100% Rust".to_owned());
        items[1].title = Some("1000 Rusty nails".to_owned());
        store_new(&db, &feed, &items, 0).await.unwrap();
        let guids =
            |listed: Vec<Listed>| listed.into_iter().map(|l| l.item.guid).collect::<Vec<_>>();

        let all = list(
            &db,
            &Filter {
                limit: 10,
                ..Filter::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(all[0].feed_name, "Example");
        assert_eq!(guids(all), ["b", "a", "c"], "undated last");

        let search = |q| Filter {
            search: Some(q),
            limit: 10,
            ..Filter::default()
        };
        assert_eq!(guids(list(&db, &search("rust")).await.unwrap()), ["b", "a"]);
        assert_eq!(guids(list(&db, &search("0%")).await.unwrap()), ["a"]);

        let window = Filter {
            since: Some(2000),
            limit: 10,
            ..Filter::default()
        };
        assert_eq!(guids(list(&db, &window).await.unwrap()), ["b"]);
        let page = Filter {
            limit: 1,
            offset: 1,
            ..Filter::default()
        };
        assert_eq!(guids(list(&db, &page).await.unwrap()), ["a"]);
    }

    #[tokio::test]
    async fn groups_recent_items_by_feed_name() {
        let db = feed::tests::db().await;
        let mut zed = settings("https://z.example/rss");
        zed.name = "zed".to_owned();
        let zed = feed::create(&db, &zed, 0).await.unwrap().unwrap();
        let zed = feed::get(&db, zed).await.unwrap().unwrap();
        let alpha = feed_with(&db, None).await;
        store_new(&db, &zed, &[parsed(Some("z1"), None, Some(500))], 0)
            .await
            .unwrap();
        store_new(
            &db,
            &alpha,
            &[
                parsed(Some("a1"), None, Some(100)),
                parsed(Some("a2"), None, Some(900)),
                parsed(Some("old"), None, Some(1)),
            ],
            0,
        )
        .await
        .unwrap();

        let groups = grouped_by_feed(&db, 50, 1000).await.unwrap();
        let shape: Vec<(String, Vec<String>)> = groups
            .into_iter()
            .map(|(name, items)| (name, items.into_iter().map(|l| l.item.guid).collect()))
            .collect();
        assert_eq!(
            shape,
            [
                ("Example".to_owned(), vec!["a2".to_owned(), "a1".to_owned()]),
                ("zed".to_owned(), vec!["z1".to_owned()]),
            ]
        );
    }

    #[tokio::test]
    async fn purges_only_what_outlived_its_feeds_retention() {
        let db = feed::tests::db().await;
        let kept_forever = feed_with(&db, None).await;
        let mut settings = settings("https://b.example/rss");
        settings.retention_days = Some(7);
        let id = feed::create(&db, &settings, 0).await.unwrap().unwrap();
        let week = feed::get(&db, id).await.unwrap().unwrap();

        let now = 100 * DAY_MS;
        let item = [parsed(Some("x"), None, None)];
        store_new(&db, &kept_forever, &item, now - 30 * DAY_MS)
            .await
            .unwrap();
        store_new(&db, &week, &item, now - 8 * DAY_MS)
            .await
            .unwrap();
        store_new(
            &db,
            &week,
            &[parsed(Some("y"), None, None)],
            now - 6 * DAY_MS,
        )
        .await
        .unwrap();

        assert_eq!(purge_expired(&db, now).await.unwrap(), 1);
        let left: Vec<String> = sqlx::query_scalar("SELECT guid FROM items ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(left, ["x", "y"]);
    }
}
