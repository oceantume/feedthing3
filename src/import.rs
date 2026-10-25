//! `feedthing import <feedthing2's app.db>`: brings feedthing2's data over,
//! once, into a new database. Ids stay the same, so links to items and
//! feeds keep working.

use std::path::Path;

use anyhow::{Context, bail};
use sqlx::{AssertSqlSafe, Connection, SqliteConnection, SqlitePool, sqlite::SqliteConnectOptions};

use crate::content;

/// feedthing2's only migration, which its whole schema comes from.
const FEEDTHING2_SCHEMA: &str = "20260812033428";

/// Copied as they are. feeds' etag and last_modified are new and start
/// empty, so the first poll fetches every feed in full.
const TABLES: [(&str, &str); 3] = [
    (
        "feeds",
        "id, url, name, poll_interval_minutes, retention_days, fetch_full_content, enabled,
         next_poll_at, last_polled_at, last_error, created_at",
    ),
    (
        "items",
        "id, feed_id, guid, url, title, summary, full_content, full_content_fetched_at,
         full_content_error, published_at, author, ai_description, ai_tags, ai_input_tokens,
         ai_output_tokens, created_at",
    ),
    (
        "push_subscriptions",
        "id, endpoint, p256dh, auth, created_at",
    ),
];

#[derive(Debug, PartialEq)]
pub struct Counts {
    pub feeds: i64,
    pub items: i64,
    pub push_subscriptions: i64,
    /// Full pages cut down to their article.
    pub articles: usize,
}

pub async fn run(db: &SqlitePool, source: &Path) -> anyhow::Result<Counts> {
    let source = source
        .canonicalize()
        .with_context(|| format!("{} not found", source.display()))?;
    check_source(&source).await?;

    let mut conn = db.acquire().await?;
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM feeds")
        .fetch_one(&mut *conn)
        .await?;
    if existing > 0 {
        bail!("this database already has feeds; import into a new one");
    }

    // Read straight from the old file. Only SELECTs touch it.
    sqlx::query("ATTACH DATABASE ? AS old")
        .bind(source.to_string_lossy().into_owned())
        .execute(&mut *conn)
        .await?;
    let copied = copy(&mut conn).await;
    sqlx::query("DETACH DATABASE old")
        .execute(&mut *conn)
        .await?;
    let mut counts = copied?;

    counts.articles = extract_articles(&mut conn).await?;
    Ok(counts)
}

/// Opens the old database read-only and checks it's feedthing2's.
async fn check_source(path: &Path) -> anyhow::Result<()> {
    let options = SqliteConnectOptions::new().filename(path).read_only(true);
    let mut source = SqliteConnection::connect_with(&options)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let schema: Option<String> =
        sqlx::query_scalar("SELECT id FROM data_table_migrations WHERE id = ?")
            .bind(FEEDTHING2_SCHEMA)
            .fetch_optional(&mut source)
            .await
            .ok()
            .flatten();
    source.close().await?;
    if schema.is_none() {
        bail!(
            "{} isn't a feedthing2 database (no migration {FEEDTHING2_SCHEMA})",
            path.display()
        );
    }
    Ok(())
}

/// Every row, in one transaction, then checks nothing was missed.
async fn copy(conn: &mut SqliteConnection) -> anyhow::Result<Counts> {
    let mut tx = conn.begin().await?;
    let mut counts = Vec::new();
    for (table, columns) in TABLES {
        // Table and column names come from TABLES above, never from input.
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO main.{table} ({columns}) SELECT {columns} FROM old.{table} ORDER BY id"
        )))
        .execute(&mut *tx)
        .await
        .with_context(|| format!("copying {table}"))?;
        let count_in =
            |schema: &str| AssertSqlSafe(format!("SELECT count(*) FROM {schema}.{table}"));
        let copied: i64 = sqlx::query_scalar(count_in("main"))
            .fetch_one(&mut *tx)
            .await?;
        let original: i64 = sqlx::query_scalar(count_in("old"))
            .fetch_one(&mut *tx)
            .await?;
        if copied != original {
            bail!("{table}: copied {copied} of {original} rows");
        }
        counts.push(copied);
    }
    tx.commit().await?;
    Ok(Counts {
        feeds: counts[0],
        items: counts[1],
        push_subscriptions: counts[2],
        articles: 0,
    })
}

/// feedthing2 kept whole pages as full content; keep only the article, as
/// the poller does now.
async fn extract_articles(conn: &mut SqliteConnection) -> anyhow::Result<usize> {
    let pages: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT id, full_content, url FROM items WHERE full_content IS NOT NULL")
            .fetch_all(&mut *conn)
            .await?;
    let mut tx = conn.begin().await?;
    for (id, page, url) in &pages {
        sqlx::query("UPDATE items SET full_content = ? WHERE id = ?")
            .bind(content::extract(page, url.as_deref()))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(pages.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed;

    /// A feedthing2 database with a feed, two items (one with a whole page
    /// as its content) and a push subscription.
    async fn feedthing2_db(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("app.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut conn = SqliteConnection::connect_with(&options).await.unwrap();
        let schema = include_str!("../tests/fixtures/feedthing2-schema.sql");
        sqlx::raw_sql(schema).execute(&mut conn).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE data_table_migrations (id varchar(64) primary key, name varchar(255), checksum varchar(128), batch integer, applied_at timestamp);
            INSERT INTO data_table_migrations VALUES ('20260812033428', 'create_core_tables', 'x', 1, '2026-08-12');
            INSERT INTO feeds (id, url, name, retention_days, created_at) VALUES (7, 'https://a.example/rss', 'A', 30, 1000);
            INSERT INTO items (id, feed_id, guid, title, ai_tags, created_at) VALUES (41, 7, 'g1', 'One', '["rust"]', 1000);
            INSERT INTO items (id, feed_id, guid, url, full_content, created_at)
              VALUES (42, 7, 'g2', 'https://a.example/2', '<nav>Menu</nav><article><p>Story</p></article>', 1000);
            INSERT INTO push_subscriptions (endpoint, p256dh, auth, created_at) VALUES ('https://push.example/1', 'k', 'a', 1000);
            "#,
        )
        .execute(&mut conn)
        .await
        .unwrap();
        conn.close().await.unwrap();
        path
    }

    /// A file, like the real target: a connection to an in-memory database
    /// opens what it ATTACHes in memory too, empty.
    async fn target(dir: &Path) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(dir.join("feedthing.db"))
            .create_if_missing(true)
            .foreign_keys(true);
        let db = SqlitePool::connect_with(options).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        db
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("feedthing-import-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn copies_everything_with_the_same_ids() {
        let dir = temp_dir("copy");
        let source = feedthing2_db(&dir).await;
        let db = target(&dir).await;

        let counts = run(&db, &source).await.unwrap();
        assert_eq!(
            counts,
            Counts {
                feeds: 1,
                items: 2,
                push_subscriptions: 1,
                articles: 1,
            }
        );
        let feed = feed::get(&db, 7).await.unwrap().unwrap();
        assert_eq!((feed.name.as_str(), feed.retention_days), ("A", Some(30)));
        let story = crate::item::get(&db, 42).await.unwrap().unwrap().item;
        assert_eq!(story.full_content.as_deref(), Some("<p>Story</p>"));
        let one = crate::item::get(&db, 41).await.unwrap().unwrap().item;
        assert_eq!(one.tags(), ["rust"]);

        // New rows carry on after the imported ids.
        let next = feed::create(&db, &feed::tests::settings("https://b.example/rss"), 2000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next, 8);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refuses_a_database_with_feeds_or_one_that_isnt_feedthing2s() {
        let dir = temp_dir("refuse");
        let source = feedthing2_db(&dir).await;
        let db = target(&dir).await;
        run(&db, &source).await.unwrap();
        let again = run(&db, &source).await.unwrap_err();
        assert!(again.to_string().contains("already has feeds"), "{again}");

        let not_feedthing2 = dir.join("other.db");
        let options = SqliteConnectOptions::new()
            .filename(&not_feedthing2)
            .create_if_missing(true);
        SqliteConnection::connect_with(&options)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let wrong = run(&db, &not_feedthing2).await.unwrap_err();
        assert!(
            wrong.to_string().contains("isn't a feedthing2 database"),
            "{wrong}"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }
}
