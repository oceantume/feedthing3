//! Reads a feed's items out of RSS 2.0, RSS 1.0 (RDF), Atom or JSON Feed.
//!
//! Only the handful of fields the app keeps are read. Feeds in the wild are
//! often a little broken, so this reads what it can rather than rejecting a
//! whole feed over one odd element or date.

use std::borrow::Cow;

use quick_xml::{XmlVersion, events::Event, reader::Reader};
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub guid: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub author: Option<String>,
    /// Epoch milliseconds.
    pub published_at: Option<i64>,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    /// Not XML or JSON we could read at all.
    Unreadable(String),
    /// Readable, but not a kind of feed we know.
    NotAFeed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "couldn't read the feed: {why}"),
            Self::NotAFeed => f.write_str("not an RSS, Atom or JSON feed"),
        }
    }
}

impl std::error::Error for Error {}

pub fn items(body: &str) -> Result<Vec<Item>, Error> {
    let body = body.trim_start_matches('\u{feff}').trim_start();
    if body.starts_with('{') {
        return json_feed(body);
    }
    let root = Element::parse(body)?;
    match root.local() {
        "rss" => Ok(root
            .child("channel")
            .map(|channel| channel.children("item").map(rss_item).collect())
            .unwrap_or_default()),
        // RSS 1.0: items sit next to the channel, not inside it.
        "RDF" => Ok(root.children("item").map(rss_item).collect()),
        "feed" => Ok(root.children("entry").map(atom_entry).collect()),
        _ => Err(Error::NotAFeed),
    }
}

fn rss_item(item: &Element) -> Item {
    Item {
        // RSS 1.0 names each item by its rdf:about.
        guid: item
            .text_of("guid")
            .or_else(|| item.attr("about").and_then(clean)),
        url: item.text_of("link"),
        title: item.text_of("title"),
        summary: item
            .text_of("description")
            .or_else(|| item.text_of("content:encoded")),
        author: item
            .text_of("author")
            .or_else(|| item.text_of("dc:creator")),
        published_at: item
            .text_of("pubDate")
            .or_else(|| item.text_of("dc:date"))
            .and_then(|date| parse_date(&date)),
    }
}

fn atom_entry(entry: &Element) -> Item {
    Item {
        guid: entry.text_of("id"),
        url: atom_link(entry),
        title: entry.child("title").and_then(Element::content),
        summary: entry
            .child("summary")
            .and_then(Element::content)
            .or_else(|| entry.child("content").and_then(Element::content)),
        author: entry
            .child("author")
            .and_then(|author| author.text_of("name").or_else(|| author.text())),
        published_at: entry
            .text_of("published")
            .or_else(|| entry.text_of("updated"))
            .and_then(|date| parse_date(&date)),
    }
}

/// The `rel="alternate"` link (which is also what a link without `rel`
/// means), or else the first link of any kind.
fn atom_link(entry: &Element) -> Option<String> {
    let href = |link: &Element| link.attr("href").and_then(clean);
    let mut links = entry.children("link");
    links
        .clone()
        .find(|link| link.attr("rel").is_none_or(|rel| rel == "alternate"))
        .and_then(href)
        .or_else(|| links.find_map(href))
}

#[derive(Deserialize)]
struct JsonFeed {
    #[serde(default)]
    items: Vec<JsonItem>,
}

#[derive(Deserialize)]
struct JsonItem {
    id: Option<serde_json::Value>,
    url: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    content_html: Option<String>,
    content_text: Option<String>,
    date_published: Option<String>,
    date_modified: Option<String>,
    /// 1.1
    #[serde(default)]
    authors: Vec<JsonAuthor>,
    /// 1.0
    author: Option<JsonAuthor>,
}

#[derive(Deserialize)]
struct JsonAuthor {
    name: Option<String>,
}

fn json_feed(body: &str) -> Result<Vec<Item>, Error> {
    let feed: serde_json::Value =
        serde_json::from_str(body).map_err(|err| Error::Unreadable(err.to_string()))?;
    if !feed
        .get("version")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v.starts_with("https://jsonfeed.org/version/"))
    {
        return Err(Error::NotAFeed);
    }
    let feed: JsonFeed =
        serde_json::from_value(feed).map_err(|err| Error::Unreadable(err.to_string()))?;
    Ok(feed
        .items
        .into_iter()
        .map(|item| Item {
            // The spec says a string, but numbers turn up.
            guid: item.id.and_then(|id| match id {
                serde_json::Value::String(id) => clean(&id),
                serde_json::Value::Number(id) => Some(id.to_string()),
                _ => None,
            }),
            url: item.url.as_deref().and_then(clean),
            title: item.title.as_deref().and_then(clean),
            summary: [item.summary, item.content_html, item.content_text]
                .into_iter()
                .flatten()
                .find_map(|text| clean(&text)),
            author: item
                .authors
                .into_iter()
                .chain(item.author)
                .find_map(|author| author.name.as_deref().and_then(clean)),
            published_at: item
                .date_published
                .or(item.date_modified)
                .and_then(|date| parse_date(&date)),
        })
        .collect())
}

/// Trimmed, and `None` when nothing's left.
fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// RFC 3339 (Atom, JSON Feed) or RFC 2822 (RSS), as epoch milliseconds.
/// Feeds get both wrong often enough that a strict parse is only the first
/// try.
pub fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(at) = text.parse::<jiff::Timestamp>() {
        return Some(at.as_millisecond());
    }
    if let Ok(at) = jiff::fmt::rfc2822::parse(text) {
        return Some(at.timestamp().as_millisecond());
    }
    lenient_rfc2822(text)
}

/// RFC 2822 as feeds actually write it: any weekday or none, a month name of
/// any length, seconds or not, a two-digit year, a named zone or none
/// (taken as UTC).
fn lenient_rfc2822(text: &str) -> Option<i64> {
    let mut parts = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .peekable();
    if parts.peek()?.chars().all(|c| c.is_ascii_alphabetic()) {
        parts.next(); // The weekday, which some feeds get wrong.
    }
    let day: i8 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i16 = match parts.next()?.parse::<i16>().ok()? {
        y @ 0..50 => 2000 + y,
        y @ 50..100 => 1900 + y,
        y => y,
    };
    let mut clock = parts.next()?.split(':');
    let hour: i8 = clock.next()?.parse().ok()?;
    let minute: i8 = clock.next()?.parse().ok()?;
    let second: i8 = clock.next().map_or(Some(0), |s| s.parse().ok())?;
    let offset_minutes = parts.next().map_or(Some(0), zone_offset_minutes)?;

    let civil = jiff::civil::DateTime::new(year, month, day, hour, minute, second, 0).ok()?;
    let offset = jiff::tz::Offset::from_seconds(offset_minutes * 60).ok()?;
    let at = civil.to_zoned(jiff::tz::TimeZone::fixed(offset)).ok()?;
    Some(at.timestamp().as_millisecond())
}

fn month_number(name: &str) -> Option<i8> {
    let name = name.get(..3)?.to_ascii_lowercase();
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    months.iter().position(|m| *m == name).map(|i| i as i8 + 1)
}

/// `+0200`, `-05:00`, or one of RFC 2822's names (US zones and UT/GMT/Z).
fn zone_offset_minutes(zone: &str) -> Option<i32> {
    if let Some(sign) = zone.chars().next().filter(|c| matches!(c, '+' | '-')) {
        let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
        if digits.len() != 4 {
            return None;
        }
        let hours: i32 = digits[..2].parse().ok()?;
        let minutes: i32 = digits[2..].parse().ok()?;
        let total = hours * 60 + minutes;
        return Some(if sign == '-' { -total } else { total });
    }
    let hours = match zone.to_ascii_uppercase().as_str() {
        "UT" | "UTC" | "GMT" | "Z" => 0,
        "EDT" => -4,
        "EST" | "CDT" => -5,
        "CST" | "MDT" => -6,
        "MST" | "PDT" => -7,
        "PST" => -8,
        _ => return None,
    };
    Some(hours * 60)
}

/// Just enough of a document tree to read feeds from: names as written
/// (`dc:creator`), attributes, and text.
#[derive(Debug, Default)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    /// The root element. Text outside it, comments, and the like are skipped.
    /// A document that breaks off partway keeps what came before the break.
    fn parse(body: &str) -> Result<Element, Error> {
        let mut reader = Reader::from_str(body);
        let config = reader.config_mut();
        config.check_end_names = false;
        config.allow_unmatched_ends = true;

        let mut open: Vec<Element> = Vec::new();
        let mut root = None;
        loop {
            match reader.read_event() {
                Ok(Event::Start(start)) => open.push(Element::from_start(&start)),
                Ok(Event::Empty(start)) => {
                    open.push(Element::from_start(&start));
                    Element::close(&mut open, &mut root);
                }
                Ok(Event::End(_)) => Element::close(&mut open, &mut root),
                Ok(Event::Text(text)) => {
                    if let Some(current) = open.last_mut() {
                        current.text.push_str(&text.xml10_content());
                    }
                }
                Ok(Event::CData(data)) => {
                    if let Some(current) = open.last_mut() {
                        current.text.push_str(&data.xml10_content());
                    }
                }
                Ok(Event::GeneralRef(reference)) => {
                    if let Some(current) = open.last_mut() {
                        current.text.push_str(&resolve_reference(&reference));
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(err) if open.is_empty() && root.is_none() => {
                    return Err(Error::Unreadable(err.to_string()));
                }
                Err(_) => break,
            }
            if root.is_some() {
                break;
            }
        }
        while !open.is_empty() {
            Element::close(&mut open, &mut root);
        }
        root.ok_or_else(|| Error::Unreadable("no root element".to_owned()))
    }

    /// Ends the innermost open element: into its parent, or as the root.
    fn close(open: &mut Vec<Element>, root: &mut Option<Element>) {
        if let Some(done) = open.pop() {
            match open.last_mut() {
                Some(parent) => parent.children.push(done),
                None => *root = Some(done),
            }
        }
    }

    fn from_start(start: &quick_xml::events::BytesStart) -> Element {
        let attrs = start
            .attributes()
            .flatten()
            .filter_map(|attr| {
                let key = attr.key.local_name().as_ref().to_owned();
                let value = attr.normalized_value(XmlVersion::Implicit1_0).ok()?;
                Some((key, value.into_owned()))
            })
            .collect();
        Element {
            name: start.name().as_ref().to_owned(),
            attrs,
            ..Element::default()
        }
    }

    /// The name without its namespace prefix.
    fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// Prefixed names (`dc:creator`) must match as written; plain ones match
    /// with or without a prefix, so `atom:feed` is still a feed.
    fn is(&self, name: &str) -> bool {
        if name.contains(':') {
            self.name == name
        } else {
            self.local() == name
        }
    }

    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + Clone {
        self.children.iter().filter(move |child| child.is(name))
    }

    fn child<'a>(&'a self, name: &'a str) -> Option<&'a Element> {
        self.children(name).next()
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn text(&self) -> Option<String> {
        clean(&self.text)
    }

    fn text_of(&self, name: &str) -> Option<String> {
        self.child(name).and_then(Element::text)
    }

    /// An Atom text construct. `type="xhtml"` holds markup as child
    /// elements; its words are kept, its tags aren't.
    fn content(&self) -> Option<String> {
        if self.attr("type") == Some("xhtml") {
            let mut words = String::new();
            self.collect_text(&mut words);
            clean(&words)
        } else {
            self.text()
        }
    }

    fn collect_text(&self, into: &mut String) {
        into.push_str(&self.text);
        for child in &self.children {
            child.collect_text(into);
        }
    }
}

/// `&amp;` and friends, numeric references, and the HTML entity feeds use
/// most without declaring it. Anything else is kept as written.
fn resolve_reference(reference: &quick_xml::events::BytesRef) -> Cow<'static, str> {
    if let Ok(Some(c)) = reference.resolve_char_ref() {
        return Cow::Owned(c.to_string());
    }
    let name: &str = reference;
    match quick_xml::escape::resolve_predefined_entity(name) {
        Some(text) => Cow::Borrowed(text),
        None if name == "nbsp" => Cow::Borrowed("\u{a0}"),
        None => Cow::Owned(format!("&{name};")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(rfc3339: &str) -> i64 {
        rfc3339.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    fn fixture(name: &str) -> Vec<Item> {
        let path = format!("{}/tests/fixtures/feeds/{name}", env!("CARGO_MANIFEST_DIR"));
        items(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn reads_rss_2_items() {
        let items = items(
            r#"<?xml version="1.0"?>
            <rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel><title>Example</title>
              <item>
                <guid isPermaLink="true">https://example.com/post/1</guid>
                <link>https://example.com/post/1</link>
                <title>First &amp; best</title>
                <description><![CDATA[<p>A summary</p>]]></description>
                <dc:creator>Alice</dc:creator>
                <pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
              </item>
              <item><link>https://example.com/post/2</link></item>
            </channel></rss>"#,
        )
        .unwrap();
        assert_eq!(
            items[0],
            Item {
                guid: Some("https://example.com/post/1".to_owned()),
                url: Some("https://example.com/post/1".to_owned()),
                title: Some("First & best".to_owned()),
                summary: Some("<p>A summary</p>".to_owned()),
                author: Some("Alice".to_owned()),
                published_at: Some(ms("2024-01-01T00:00:00Z")),
            }
        );
        assert_eq!(
            items[1],
            Item {
                url: Some("https://example.com/post/2".to_owned()),
                ..Item::default()
            }
        );
    }

    #[test]
    fn reads_atom_entries_and_picks_the_alternate_link() {
        let items = items(
            r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry>
                <id>urn:uuid:atom-1</id>
                <link rel="replies" href="https://example.com/atom/1#comments"/>
                <link href="https://example.com/atom/1"/>
                <title type="html">Atom &lt;b&gt;post&lt;/b&gt;</title>
                <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml">Hello <b>there</b></div></content>
                <author><name>Bob</name></author>
                <updated>2024-06-01T00:00:00+02:00</updated>
            </entry></feed>"#,
        )
        .unwrap();
        assert_eq!(
            items[0],
            Item {
                guid: Some("urn:uuid:atom-1".to_owned()),
                url: Some("https://example.com/atom/1".to_owned()),
                title: Some("Atom <b>post</b>".to_owned()),
                summary: Some("Hello there".to_owned()),
                author: Some("Bob".to_owned()),
                published_at: Some(ms("2024-05-31T22:00:00Z")),
            }
        );
    }

    #[test]
    fn reads_rss_1_items() {
        let items = items(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/">
              <channel rdf:about="https://example.com/"><title>Example</title></channel>
              <item rdf:about="https://example.com/1">
                <title>One</title><link>https://example.com/1</link>
                <dc:date>2024-03-01T12:00:00Z</dc:date>
              </item>
            </rdf:RDF>"#,
        )
        .unwrap();
        assert_eq!(items[0].guid.as_deref(), Some("https://example.com/1"));
        assert_eq!(items[0].title.as_deref(), Some("One"));
        assert_eq!(items[0].published_at, Some(ms("2024-03-01T12:00:00Z")));
    }

    #[test]
    fn reads_json_feed_items() {
        let items = items(
            r#"{"version": "https://jsonfeed.org/version/1.1", "title": "Example", "items": [
                {"id": 7, "url": "https://example.com/7", "title": "Seven", "content_html": "<p>Hi</p>",
                 "authors": [{"name": "Cy"}], "date_published": "2024-02-02T10:00:00-05:00"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            items[0],
            Item {
                guid: Some("7".to_owned()),
                url: Some("https://example.com/7".to_owned()),
                title: Some("Seven".to_owned()),
                summary: Some("<p>Hi</p>".to_owned()),
                author: Some("Cy".to_owned()),
                published_at: Some(ms("2024-02-02T15:00:00Z")),
            }
        );
    }

    #[test]
    fn tells_unknown_documents_from_unreadable_ones() {
        assert_eq!(items("<root><data/></root>"), Err(Error::NotAFeed));
        assert_eq!(items(r#"{"items": []}"#), Err(Error::NotAFeed));
        assert!(matches!(items("<html"), Err(Error::Unreadable(_))));
        assert!(matches!(items("{nope"), Err(Error::Unreadable(_))));
    }

    #[test]
    fn keeps_the_items_before_a_document_breaks_off() {
        let items =
            items("<rss><channel><item><title>Kept</title></item><item><title>Cut").unwrap();
        assert_eq!(items[0].title.as_deref(), Some("Kept"));
    }

    #[test]
    fn reads_dates_the_way_feeds_write_them() {
        let noon_utc = ms("2024-01-05T12:00:00Z");
        for written in [
            "2024-01-05T12:00:00Z",
            "2024-01-05T07:00:00-05:00",
            "Fri, 05 Jan 2024 12:00:00 GMT",
            "Fri, 05 Jan 2024 12:00:00 +0000",
            // Wrong weekday.
            "Mon, 05 Jan 2024 12:00:00 GMT",
            // No weekday, no seconds, a named zone.
            "5 Jan 2024 07:00 EST",
            // Full month name, two-digit year, extra spaces.
            "Friday,  05 January 24 12:00:00 UT",
            // A colon in the offset, no zone at all.
            "Fri, 05 Jan 2024 13:00:00 +01:00",
            "Fri, 05 Jan 2024 12:00:00",
        ] {
            assert_eq!(parse_date(written), Some(noon_utc), "{written}");
        }
        assert_eq!(parse_date("yesterday"), None);
        assert_eq!(parse_date("Fri, 32 Jan 2024 12:00:00 GMT"), None);
    }

    #[test]
    fn reads_the_hacker_news_fixture() {
        let items = fixture("hn.xml");
        assert!(!items.is_empty());
        assert!(
            items
                .iter()
                .all(|item| item.url.is_some() && item.author.is_none())
        );
        assert_eq!(
            items[0].url.as_deref(),
            Some("https://cacm.acm.org/research/a-decade-of-docker-containers/")
        );
        assert_eq!(
            items[0].title.as_deref(),
            Some("A Decade of Docker Containers")
        );
        assert!(items.iter().all(|item| item.published_at.is_some()));
    }

    #[test]
    fn reads_the_lobsters_fixture() {
        let items = fixture("lobsters.xml");
        assert!(
            items
                .iter()
                .all(|item| item.author.is_some() && item.published_at.is_some())
        );
        assert_eq!(items[0].guid.as_deref(), Some("https://lobste.rs/s/azkace"));
        assert_eq!(items[0].author.as_deref(), Some("ifixit.com via Signez"));
    }

    #[test]
    fn reads_the_javascript_jabber_podcast_fixture() {
        let items = fixture("javascript-jabber.xml");
        assert!(
            items
                .iter()
                .all(|item| item.url.is_some() && item.published_at.is_some())
        );
        assert_eq!(
            items[0].guid.as_deref(),
            Some("https://api.spreaker.com/episode/70275018")
        );
        assert_eq!(
            items[0].title.as_deref(),
            Some(
                "Mongoose 9, AI-Powered Database Tools & the Future of Server-Side JavaScript with Val Karpov - JSJ 703"
            )
        );
    }

    #[test]
    fn reads_the_planet_gnome_atom_fixture() {
        let items = fixture("planet-gnome.xml");
        assert!(items.iter().all(|item| {
            item.url
                .as_deref()
                .is_some_and(|url| url.starts_with("http"))
                && item.published_at.is_some()
        }));
        assert_eq!(
            items[0].guid.as_deref(),
            Some("https://www.aryank.in/posts/2025-12-21-introducing-open-forms/")
        );
        assert_eq!(items[0].author.as_deref(), Some("Aryan Kaushik"));
    }
}
