//! HTML from feeds and the pages they link to: untrusted, so it's always
//! sanitised before it's shown, and article pages are cut down to the
//! article.

use std::collections::{HashMap, HashSet};

use ammonia::{Builder, Url, UrlRelative};

/// What feed HTML may keep, as in feedthing2. Links open in a new tab.
/// Relative URLs are resolved against `base` (the item's page) when there's
/// one, and dropped otherwise, since they'd point into this app.
pub fn sanitize(html: &str, base: Option<&str>) -> String {
    let mut builder = Builder::default();
    builder
        .tags(HashSet::from([
            "a",
            "b",
            "strong",
            "i",
            "em",
            "code",
            "br",
            "p",
            "ul",
            "ol",
            "li",
            "blockquote",
            "pre",
            "img",
            "h1",
            "h2",
            "h3",
            "h4",
        ]))
        .generic_attributes(HashSet::new())
        .tag_attributes(HashMap::from([
            ("a", HashSet::from(["href"])),
            ("img", HashSet::from(["src", "alt"])),
        ]))
        .set_tag_attribute_value("a", "target", "_blank")
        .link_rel(Some("noopener noreferrer"))
        .url_schemes(HashSet::from(["http", "https", "mailto"]))
        .url_relative(relative_urls(base));
    builder.clean(html).to_string().trim().to_owned()
}

/// The article in a whole web page, sanitised: what's in its `<article>`
/// (the one with the most text) or `<main>`, or else the page without its
/// navigation, header, footer, sidebars and forms.
pub fn extract(page: &str, base: Option<&str>) -> String {
    // First pass: only the tags this needs, without attributes, so the
    // output is balanced HTML in which `<article>` can't mean anything else.
    let mut builder = Builder::default();
    builder
        .tags(HashSet::from([
            "article",
            "main",
            "a",
            "b",
            "strong",
            "i",
            "em",
            "code",
            "br",
            "p",
            "ul",
            "ol",
            "li",
            "blockquote",
            "pre",
            "img",
            "h1",
            "h2",
            "h3",
            "h4",
        ]))
        .clean_content_tags(HashSet::from([
            "script", "style", "template", "noscript", "nav", "header", "footer", "aside", "form",
            "button", "select", "svg", "iframe", "dialog",
        ]))
        .generic_attributes(HashSet::new())
        .tag_attributes(HashMap::from([
            ("a", HashSet::from(["href"])),
            ("img", HashSet::from(["src", "alt"])),
        ]))
        .link_rel(None)
        .url_relative(relative_urls(base));
    let page = builder.clean(page).to_string();

    let body = largest_region(&page, "article")
        .or_else(|| largest_region(&page, "main"))
        .unwrap_or(&page);
    sanitize(body, base)
}

fn relative_urls(base: Option<&str>) -> UrlRelative<'static> {
    match base.and_then(|base| Url::parse(base).ok()) {
        Some(base) => UrlRelative::RewriteWithBase(base),
        None => UrlRelative::Deny,
    }
}

/// The inside of the `<tag>…</tag>` with the most text, in HTML where the
/// tag has no attributes. Nested ones count as part of the outer one.
fn largest_region<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut regions = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut at = 0;
    while let Some(found) = html[at..].find('<') {
        let i = at + found;
        if html[i..].starts_with(&open) {
            if depth == 0 {
                start = i + open.len();
            }
            depth += 1;
            at = i + open.len();
        } else if html[i..].starts_with(&close) {
            if depth == 1 {
                regions.push(&html[start..i]);
            }
            depth = (depth - 1).max(0);
            at = i + close.len();
        } else {
            at = i + 1;
        }
    }
    regions.into_iter().max_by_key(|region| text_len(region))
}

/// Roughly how much text there is, ignoring tags.
fn text_len(html: &str) -> usize {
    let mut in_tag = false;
    html.chars()
        .filter(|c| match c {
            '<' => {
                in_tag = true;
                false
            }
            '>' => {
                in_tag = false;
                false
            }
            c => !in_tag && !c.is_whitespace(),
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_safe_tags_and_attributes() {
        let html = r#"<p class="x" onclick="evil()">Hi <script>alert(1)</script><a href="https://a.example/" title="t">link</a>
            <img src="https://a.example/i.png" alt="pic" width="3"> <a href="javascript:alert(1)">bad</a>
            <iframe src="https://a.example/"></iframe><span>text</span></p>"#;
        assert_eq!(
            sanitize(html, None),
            r#"<p>Hi <a href="https://a.example/" target="_blank" rel="noopener noreferrer">link</a>
            <img src="https://a.example/i.png" alt="pic"> <a target="_blank" rel="noopener noreferrer">bad</a>
            text</p>"#
        );
    }

    #[test]
    fn resolves_relative_urls_against_the_items_page() {
        assert_eq!(
            sanitize(
                r#"<img src="/i.png"><a href="next">n</a>"#,
                Some("https://a.example/posts/1")
            ),
            r#"<img src="https://a.example/i.png"><a href="https://a.example/posts/next" target="_blank" rel="noopener noreferrer">n</a>"#
        );
        assert_eq!(sanitize(r#"<img src="/i.png">"#, None), "<img>");
    }

    #[test]
    fn extracts_the_article_with_the_most_text() {
        let page = r#"<!doctype html><html><head><title>T</title><style>p{}</style></head><body>
            <header><nav><a href="/">Home</a></nav></header>
            <article><p>Related: short</p></article>
            <article class="post"><header><h1>Title</h1></header><p>The <em>real</em> story, at length.</p>
              <aside>Subscribe!</aside></article>
            <footer>© 2026</footer></body></html>"#;
        assert_eq!(
            extract(page, None),
            "<p>The <em>real</em> story, at length.</p>"
        );
    }

    #[test]
    fn falls_back_to_main_then_to_the_page_without_its_chrome() {
        let page = "<body><nav>Menu</nav><main><p>Main text</p></main><footer>Foot</footer></body>";
        assert_eq!(extract(page, None), "<p>Main text</p>");
        let page =
            "<body><header>Site</header><div><p>Just text</p></div><form><input></form></body>";
        assert_eq!(extract(page, None), "<p>Just text</p>");
    }
}
