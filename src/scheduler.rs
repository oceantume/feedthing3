//! The background work: polling feeds as they come due, and deleting items
//! past their feed's retention. Runs inside the server's process.
//!
//! Each feed's next poll is kept on its row, so a restart picks the schedule
//! up where it left off instead of polling everything at once.

use std::time::Duration;

use sqlx::SqlitePool;
use tokio::{task::JoinHandle, time::MissedTickBehavior};

use crate::{
    clock, feed,
    item::{self, Item},
    poller::{self, Outcome},
    shutdown,
};

const POLL_CHECK_EVERY: Duration = Duration::from_secs(30);
const PURGE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct Scheduler {
    pub db: SqlitePool,
    pub http: reqwest::Client,
}

impl Scheduler {
    /// Purges, polls what's due, then keeps doing both on their intervals
    /// until shutdown. A poll in progress at shutdown is dropped: its feed
    /// was booked for its next poll already, and items are saved in one
    /// transaction, so nothing is left half done.
    pub fn spawn(self, stop: shutdown::Signal) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut purge_tick = tokio::time::interval(PURGE_EVERY);
            let mut poll_tick = tokio::time::interval(POLL_CHECK_EVERY);
            poll_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                let work = async {
                    tokio::select! {
                        biased;
                        _ = purge_tick.tick() => self.purge().await,
                        _ = poll_tick.tick() => self.poll_due().await,
                    }
                };
                tokio::select! {
                    () = shutdown::requested(stop.clone()) => break,
                    () = work => {}
                }
            }
        })
    }

    async fn purge(&self) {
        match item::purge_expired(&self.db, clock::now_ms()).await {
            Ok(0) => {}
            Ok(deleted) => tracing::info!("retention purge: deleted {deleted} item(s)"),
            Err(err) => tracing::error!(%err, "retention purge failed"),
        }
    }

    async fn poll_due(&self) {
        let due = match feed::due(&self.db, clock::now_ms()).await {
            Ok(due) => due,
            Err(err) => return tracing::error!(%err, "listing due feeds failed"),
        };
        if due.is_empty() {
            return;
        }
        let (mut new, mut failed) = (0, 0);
        for feed in &due {
            match poller::poll(&self.db, &self.http, feed, clock::now_ms()).await {
                Ok(Outcome::Polled { new_items, first }) => {
                    new += new_items.len();
                    if !first && !new_items.is_empty() {
                        self.on_new_items(feed, new_items).await;
                    }
                }
                Ok(Outcome::Failed(error)) => {
                    failed += 1;
                    tracing::warn!(feed = feed.id, error, "poll failed");
                }
                Ok(Outcome::Skipped | Outcome::Unchanged) => {}
                Err(err) => {
                    failed += 1;
                    tracing::error!(feed = feed.id, %err, "saving a poll failed");
                }
            }
        }
        tracing::info!(
            "poll tick: {} feed(s) checked, {new} new item(s), {failed} error(s)",
            due.len()
        );
    }

    /// News from a feed that was polled before: the first poll's backlog
    /// isn't.
    async fn on_new_items(&self, feed: &feed::Feed, items: Vec<Item>) {
        tracing::debug!(feed = feed.id, count = items.len(), "new items");
    }
}
