//! The client for every outgoing call: feeds, article pages, the AI and push
//! services.

use std::time::Duration;

/// Some servers turn away requests without one, and it tells feed owners
/// who's polling them.
const USER_AGENT: &str = concat!("feedthing/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// `timeout` covers the whole call. A feed that never answers would otherwise
/// hold up every feed polled after it, and shutdown.
pub fn new(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .build()
        .expect("building the HTTP client")
}
