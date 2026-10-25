use jiff::tz::TimeZone;
use maud::{Markup, PreEscaped, html};

use super::Page;
use crate::{
    content,
    item::{Item, Listed},
    link,
    web::{layout, time},
};

pub fn index(items: &[Listed], page: &Page, now: i64, tz: &TimeZone) -> Markup {
    let title = match page.feed {
        Some(feed) => format!("Items — {}", feed.name),
        None => "Items".to_owned(),
    };
    layout::page(
        &title,
        html! {
            @if let Some(feed) = page.feed {
                h1 { "Items in " (feed.name) }
                a.back href="/items" { "← All items" }
            } @else {
                h1 { "All items" }
            }
            form.search method="get" action="/items" {
                @if let Some(feed) = page.feed {
                    input type="hidden" name="feed_id" value=(feed.id);
                }
                input name="q" type="search" placeholder="Search titles…" value=(page.search);
                button { "Search" }
            }
            @if items.is_empty() {
                p.muted { "No items found." }
            } @else {
                ul.list.items {
                    @for listed in items {
                        li {
                            a.item-title href={ "/items/" (listed.item.id) } { (title_of(&listed.item)) }
                            div.meta {
                                span {
                                    @if page.feed.is_none() { (listed.feed_name) " · " }
                                    @if let Some(at) = listed.item.published_at { (time::item_time(at, now, tz)) }
                                    @if let Some(author) = &listed.item.author { " · " (author) }
                                }
                            }
                            (description_and_tags(&listed.item))
                        }
                    }
                }
            }
            (pager(page))
        },
    )
}

fn pager(page: &Page) -> Markup {
    let has_newer = page.offset > 0;
    if !has_newer && !page.has_more {
        return html! {};
    }
    let href = |offset: i64| {
        let mut href = format!("/items?offset={offset}");
        if let Some(feed) = page.feed {
            href.push_str(&format!("&feed_id={}", feed.id));
        }
        if !page.search.is_empty() {
            href.push_str("&q=");
            href.push_str(&encode_query_value(page.search));
        }
        if page.limit != crate::item::DEFAULT_PAGE {
            href.push_str(&format!("&limit={}", page.limit));
        }
        href
    };
    html! {
        nav.pager {
            @if has_newer { a href=(href((page.offset - page.limit).max(0))) { "← Newer" } } @else { span {} }
            @if page.has_more { a href=(href(page.offset + page.limit)) { "Older →" } }
        }
    }
}

pub fn show(listed: &Listed, now: i64, tz: &TimeZone) -> Markup {
    let item = &listed.item;
    let content = item
        .full_content
        .as_deref()
        .or(item.summary.as_deref())
        .map(|html| content::sanitize(html, item.url.as_deref()))
        .filter(|html| !html.is_empty());
    layout::page(
        item.title.as_deref().unwrap_or("Item"),
        html! {
            a.back href={ "/items?feed_id=" (item.feed_id) } { "← " (listed.feed_name) }
            h1 { (title_of(item)) }
            div.meta {
                span {
                    @if let Some(at) = item.published_at { (time::item_time(at, now, tz)) }
                    @if let Some(author) = &item.author { " · " (author) }
                }
                @if let Some(url) = &item.url {
                    a href=(link::outbound(url)) target="_blank" rel="noopener noreferrer" { "Open original →" }
                }
            }
            (description_and_tags(item))
            @if let Some(error) = &item.full_content_error {
                p.muted { "Couldn't fetch the full article: " (error) }
            }
            @if let Some(content) = content {
                // Sanitised just above, so safe to include as HTML.
                div.content { (PreEscaped(content)) }
            }
        },
    )
}

/// What to call an item that may have no title.
pub fn title_of(item: &Item) -> &str {
    item.title
        .as_deref()
        .or(item.url.as_deref())
        .unwrap_or("Untitled")
}

/// The AI's take on an item, when it has one.
pub fn description_and_tags(item: &Item) -> Markup {
    let tags = item.tags();
    html! {
        @if let Some(description) = &item.ai_description {
            p.item-description { (description) }
        }
        @if !tags.is_empty() {
            ul.tags { @for tag in &tags { li.tag { (tag) } } }
        }
    }
}

/// Percent-encodes a query parameter's value.
fn encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::encode_query_value;

    #[test]
    fn encodes_search_terms_for_links() {
        assert_eq!(encode_query_value("rust & go"), "rust%20%26%20go");
        assert_eq!(encode_query_value("café"), "caf%C3%A9");
    }
}
