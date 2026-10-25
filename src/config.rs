//! Settings from the environment. Debug builds fall back to values that work
//! on a laptop; release builds refuse to start on a value they can't read.

use std::{env, time::Duration};

use anyhow::{Context, bail};
use jiff::tz::TimeZone;

pub struct Config {
    pub bind: String,
    pub database_url: String,
    /// For calls to feeds and pages, which can be slow or never answer.
    pub fetch_timeout: Duration,
    /// Where "today" and "yesterday" are, for showing times.
    pub tz: TimeZone,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let fetch_timeout_ms = match env::var("FETCH_TIMEOUT_MS") {
            Ok(ms) => ms
                .parse()
                .with_context(|| format!("FETCH_TIMEOUT_MS={ms:?} isn't a number"))?,
            Err(_) => 10_000,
        };
        let tz_name = env::var("TZ").unwrap_or_else(|_| "America/Toronto".to_owned());
        let tz = match TimeZone::get(&tz_name) {
            Ok(tz) => tz,
            Err(err) if cfg!(debug_assertions) => {
                tracing::warn!(%err, tz_name, "unknown time zone; using UTC");
                TimeZone::UTC
            }
            Err(err) => bail!("TZ={tz_name:?}: {err}"),
        };
        Ok(Self {
            bind: env::var("BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_owned()),
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://feedthing.db".to_owned()),
            fetch_timeout: Duration::from_millis(fetch_timeout_ms),
            tz,
        })
    }
}
