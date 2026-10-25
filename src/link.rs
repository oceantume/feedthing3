//! Where links to an item's original page go. Some sites read better
//! through a mirror that skips their login wall.

/// The URL to send people to for `url`.
pub fn outbound(url: &str) -> String {
    for rewrite in [x_to_xcancel] {
        if let Some(rewritten) = rewrite(url) {
            return rewritten;
        }
    }
    url.to_owned()
}

/// x.com and twitter.com (with or without www.) → xcancel.com.
fn x_to_xcancel(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    matches!(host, "x.com" | "twitter.com").then(|| format!("{scheme}://xcancel.com/{path}"))
}

#[cfg(test)]
mod tests {
    use super::outbound;

    #[test]
    fn sends_x_and_twitter_links_through_xcancel() {
        assert_eq!(
            outbound("https://x.com/a/status/1"),
            "https://xcancel.com/a/status/1"
        );
        assert_eq!(outbound("http://www.Twitter.com/a"), "http://xcancel.com/a");
    }

    #[test]
    fn leaves_everything_else_alone() {
        for url in [
            "https://example.com/x.com/",
            "https://notx.com/a",
            "https://x.com.evil.example/a",
            "https://x.com",
            "mailto:a@x.com/",
        ] {
            assert_eq!(outbound(url), url);
        }
    }
}
