//! Adding, editing and deleting feeds.

mod views;

use axum::{
    Form, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use serde::Deserialize;

use super::{AppState, error::AppError};
use crate::{
    clock,
    feed::{self, Feed, Settings},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/feeds", get(index).post(create))
        .route("/feeds/{id}", get(show).post(update))
        .route("/feeds/{id}/delete", get(confirm_delete).post(destroy))
}

/// `GET /feeds`
async fn index(State(app): State<AppState>) -> Result<Response, AppError> {
    let feeds = feed::list_with_counts(&app.db).await?;
    Ok(views::index(
        &feeds,
        &FeedForm::new_feed(),
        &Errors::default(),
        clock::now_ms(),
    )
    .into_response())
}

/// The add and edit forms, as sent. Every field is text so that a bad value
/// can be shown back as it was typed; checkboxes are only sent when ticked.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FeedForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub poll_interval_minutes: String,
    #[serde(default)]
    pub retention_days: String,
    pub fetch_full_content: Option<String>,
    pub enabled: Option<String>,
}

/// `POST /feeds`
async fn create(
    State(app): State<AppState>,
    Form(form): Form<FeedForm>,
) -> Result<Response, AppError> {
    let errors = match form.settings() {
        Ok(settings) => match feed::create(&app.db, &settings, clock::now_ms()).await? {
            Ok(_) => return Ok(Redirect::to("/feeds").into_response()),
            Err(feed::DuplicateUrl) => Errors::duplicate_url(),
        },
        Err(errors) => errors,
    };
    let status = if errors == Errors::duplicate_url() {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_REQUEST
    };
    let feeds = feed::list_with_counts(&app.db).await?;
    Ok((
        status,
        views::index(&feeds, &form, &errors, clock::now_ms()),
    )
        .into_response())
}

/// Which message to show after a backfill (`POST /feeds/{id}/enrich`).
#[derive(Debug, Default, Deserialize)]
pub struct ShowQuery {
    pub enriched: Option<u32>,
    pub ai_unavailable: Option<u8>,
    pub ai_failed: Option<u8>,
}

/// `GET /feeds/{id}`
async fn show(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ShowQuery>,
) -> Result<Response, AppError> {
    let feed = find(&app, &id).await?;
    Ok(views::show(&feed, &FeedForm::from(&feed), &Errors::default(), &query).into_response())
}

/// `POST /feeds/{id}`
async fn update(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<FeedForm>,
) -> Result<Response, AppError> {
    let feed = find(&app, &id).await?;
    let rejected = |status, errors: Errors| {
        (
            status,
            views::show(&feed, &form, &errors, &ShowQuery::default()),
        )
            .into_response()
    };
    let settings = match form.settings() {
        Ok(settings) => settings,
        Err(errors) => return Ok(rejected(StatusCode::BAD_REQUEST, errors)),
    };
    match feed::update(&app.db, feed.id, &settings).await? {
        Ok(()) => Ok(Redirect::to(&format!("/feeds/{}", feed.id)).into_response()),
        Err(feed::DuplicateUrl) => Ok(rejected(StatusCode::CONFLICT, Errors::duplicate_url())),
    }
}

/// `GET /feeds/{id}/delete`
async fn confirm_delete(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let feed = find(&app, &id).await?;
    Ok(views::confirm_delete(&feed).into_response())
}

/// `POST /feeds/{id}/delete`
async fn destroy(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let feed = find(&app, &id).await?;
    feed::delete(&app.db, feed.id).await?;
    Ok(Redirect::to("/feeds").into_response())
}

/// An id that isn't a number is as missing as one that isn't there.
async fn find(app: &AppState, id: &str) -> Result<Feed, AppError> {
    let id = id.parse().map_err(|_| AppError::NotFound)?;
    feed::get(&app.db, id).await?.ok_or(AppError::NotFound)
}

/// What's wrong with each field, if anything.
#[derive(Debug, Default, PartialEq)]
pub struct Errors {
    pub name: Option<&'static str>,
    pub url: Option<&'static str>,
    pub poll_interval_minutes: Option<&'static str>,
    pub retention_days: Option<&'static str>,
}

impl Errors {
    fn duplicate_url() -> Self {
        Self {
            url: Some("A feed with this URL already exists."),
            ..Self::default()
        }
    }
}

impl FeedForm {
    /// What the add form starts with.
    fn new_feed() -> Self {
        Self {
            enabled: Some("on".to_owned()),
            ..Self::default()
        }
    }

    fn settings(&self) -> Result<Settings, Errors> {
        let mut errors = Errors::default();
        let name = self.name.trim();
        if name.is_empty() {
            errors.name = Some("Give the feed a name.");
        }
        let url = self.url.trim();
        if !is_web_url(url) {
            errors.url = Some("Enter a full http:// or https:// address.");
        }
        let poll_interval_minutes = match self.poll_interval_minutes.trim() {
            "" => Some(15),
            minutes => positive(minutes),
        };
        if poll_interval_minutes.is_none() {
            errors.poll_interval_minutes = Some("Must be a positive whole number.");
        }
        let retention_days = match self.retention_days.trim() {
            "" => Some(None),
            days => positive(days).map(Some),
        };
        if retention_days.is_none() {
            errors.retention_days = Some("Must be empty or a positive whole number.");
        }
        match (poll_interval_minutes, retention_days) {
            (Some(poll_interval_minutes), Some(retention_days)) if errors == Errors::default() => {
                Ok(Settings {
                    name: name.to_owned(),
                    url: url.to_owned(),
                    poll_interval_minutes,
                    retention_days,
                    fetch_full_content: self.fetch_full_content.is_some(),
                    enabled: self.enabled.is_some(),
                })
            }
            _ => Err(errors),
        }
    }
}

impl From<&Feed> for FeedForm {
    fn from(feed: &Feed) -> Self {
        let on = |set: bool| set.then(|| "on".to_owned());
        Self {
            name: feed.name.clone(),
            url: feed.url.clone(),
            poll_interval_minutes: feed.poll_interval_minutes.to_string(),
            retention_days: feed
                .retention_days
                .map(|d| d.to_string())
                .unwrap_or_default(),
            fetch_full_content: on(feed.fetch_full_content),
            enabled: on(feed.enabled),
        }
    }
}

fn positive(text: &str) -> Option<i64> {
    text.parse().ok().filter(|n| *n > 0)
}

fn is_web_url(text: &str) -> bool {
    reqwest::Url::parse(text)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(name: &str, url: &str, poll: &str, retention: &str) -> FeedForm {
        FeedForm {
            name: name.to_owned(),
            url: url.to_owned(),
            poll_interval_minutes: poll.to_owned(),
            retention_days: retention.to_owned(),
            fetch_full_content: None,
            enabled: Some("on".to_owned()),
        }
    }

    #[test]
    fn reads_a_good_form_with_defaults_for_blank_numbers() {
        let settings = form(" HN ", "https://news.ycombinator.com/rss", "", "")
            .settings()
            .unwrap();
        assert_eq!(settings.name, "HN");
        assert_eq!(settings.poll_interval_minutes, 15);
        assert_eq!(settings.retention_days, None);
        assert!(settings.enabled);
        assert!(!settings.fetch_full_content);
    }

    #[test]
    fn names_each_bad_field() {
        let errors = form("", "ftp://example.com", "0", "-3")
            .settings()
            .unwrap_err();
        assert!(errors.name.is_some());
        assert!(errors.url.is_some());
        assert!(errors.poll_interval_minutes.is_some());
        assert!(errors.retention_days.is_some());
        assert!(
            form("x", "not a url", "15", "")
                .settings()
                .unwrap_err()
                .url
                .is_some()
        );
    }
}
