//! The shell shared by every full page.

use maud::{DOCTYPE, Markup, html};

use super::assets;

pub fn page(title: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="theme-color" content="#2dacf9";
                title { (title) }
                link rel="icon" type="image/svg+xml" href=(assets::url("favicon.svg"));
                link rel="manifest" href=(assets::url("manifest.json"));
                link rel="stylesheet" href=(assets::url("app.css"));
            }
            body {
                main.page {
                    nav.site-nav {
                        div.site-nav-links {
                            a href="/" { "Summary" }
                            a href="/feeds" { "Feeds" }
                        }
                    }
                    (content)
                }
            }
        }
    }
}
