use maud::{Markup, html};

use super::{Errors, FeedForm, ShowQuery};
use crate::{
    feed::{Feed, WithCount},
    web::{layout, time},
};

pub fn index(feeds: &[WithCount], form: &FeedForm, errors: &Errors, now: i64) -> Markup {
    layout::page(
        "Feeds",
        html! {
            h1 { "Feeds" }
            section.card {
                h2 { "Add feed" }
                form.stack method="post" action="/feeds" {
                    (name_and_url(form, errors))
                    (check("enabled", "Enabled", form.enabled.is_some()))
                    div { button { "Add feed" } }
                }
            }
            section.section {
                h2 { (feeds.len()) " feed" @if feeds.len() != 1 { "s" } }
                @if feeds.is_empty() {
                    p.muted { "No feeds yet." }
                } @else {
                    ul.list {
                        @for row in feeds { (feed_row(row, now)) }
                    }
                }
            }
        },
    )
}

fn feed_row(row: &WithCount, now: i64) -> Markup {
    let feed = &row.feed;
    html! {
        li.card {
            div.row {
                a.feed-link href={ "/feeds/" (feed.id) } {
                    strong { (feed.name) }
                    small.muted.ellipsis { (feed.url) }
                }
                @if !feed.enabled { small.muted { "disabled" } }
            }
            div.meta {
                a href={ "/items?feed_id=" (feed.id) } { (row.item_count) " items" }
                span { "polled: " (time::relative(feed.last_polled_at, now)) }
                span { "next: " (time::next_poll(feed.last_polled_at, feed.poll_interval_minutes, now)) }
                @if feed.last_error.is_some() { span.danger { "error" } }
            }
        }
    }
}

pub fn show(feed: &Feed, form: &FeedForm, errors: &Errors, query: &ShowQuery) -> Markup {
    layout::page(
        &feed.name,
        html! {
            a.back href="/feeds" { "← Back to feeds" }
            h1 { (feed.name) }
            section.card {
                form.stack method="post" action={ "/feeds/" (feed.id) } {
                    (name_and_url(form, errors))
                    label.field {
                        "Poll interval (minutes)"
                        input name="poll_interval_minutes" type="number" min="1" required
                            value=(form.poll_interval_minutes);
                        (field_error(errors.poll_interval_minutes))
                    }
                    label.field {
                        "Retention (days, blank for unlimited)"
                        input name="retention_days" type="number" min="1" value=(form.retention_days);
                        (field_error(errors.retention_days))
                    }
                    (check("fetch_full_content", "Fetch full content", form.fetch_full_content.is_some()))
                    (check("enabled", "Enabled", form.enabled.is_some()))
                    @if let Some(error) = &feed.last_error {
                        p.field-error { "Last poll error: " (error) }
                    }
                    div { button { "Save changes" } }
                }
            }
            section.card {
                h2 { "AI summaries" }
                form method="post" action={ "/feeds/" (feed.id) "/enrich" } {
                    button { "Backfill AI summaries" }
                }
                @if let Some(notice) = backfill_notice(query) {
                    p.muted { (notice) }
                }
            }
            p.section {
                a.danger href={ "/feeds/" (feed.id) "/delete" } { "Delete this feed" }
            }
        },
    )
}

fn backfill_notice(query: &ShowQuery) -> Option<String> {
    if query.ai_unavailable.is_some() {
        return Some("AI isn't configured, so summaries can't be generated.".to_owned());
    }
    if query.ai_failed.is_some() {
        return Some("The AI service didn't answer as expected. Try again in a bit.".to_owned());
    }
    match query.enriched? {
        0 => Some("All items already have summaries.".to_owned()),
        1 => Some("Backfilled 1 item.".to_owned()),
        n => Some(format!("Backfilled {n} items.")),
    }
}

pub fn confirm_delete(feed: &Feed) -> Markup {
    layout::page(
        &format!("Delete {}", feed.name),
        html! {
            section.card {
                h2 { "Delete \u{201c}" (feed.name) "\u{201d}?" }
                p { "This permanently deletes the feed and all of its items. This cannot be undone." }
                div.actions {
                    form method="post" action={ "/feeds/" (feed.id) "/delete" } {
                        button.danger { "Yes, delete" }
                    }
                    a.button href={ "/feeds/" (feed.id) } { "Cancel" }
                }
            }
        },
    )
}

fn name_and_url(form: &FeedForm, errors: &Errors) -> Markup {
    html! {
        label.field {
            "Name"
            input name="name" required placeholder="Hacker News" value=(form.name);
            (field_error(errors.name))
        }
        label.field {
            "URL"
            input name="url" type="url" required placeholder="https://news.ycombinator.com/rss"
                value=(form.url);
            (field_error(errors.url))
        }
    }
}

fn check(name: &str, label: &str, checked: bool) -> Markup {
    html! {
        label.check {
            input type="checkbox" name=(name) checked[checked];
            (label)
        }
    }
}

fn field_error(error: Option<&str>) -> Markup {
    html! {
        @if let Some(error) = error { p.field-error { (error) } }
    }
}
