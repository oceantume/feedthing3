//! The shell shared by every full page.

use maud::{DOCTYPE, Markup, PreEscaped, html};

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
                script type="module" src=(assets::url("app.mjs")) {}
            }
            body {
                main.page {
                    nav.site-nav {
                        div.site-nav-links {
                            a href="/" { "Summary" }
                            a href="/feeds" { "Feeds" }
                        }
                        // Shown by app.mjs when the browser and the server
                        // can both do push.
                        button #push-bell .bell type="button" hidden title="Notifications" {
                            (bell_icon())
                            span { "Off" }
                        }
                    }
                    (content)
                }
            }
        }
    }
}

fn bell_icon() -> Markup {
    PreEscaped(
        r#"<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/></svg>"#
            .to_owned(),
    )
}
