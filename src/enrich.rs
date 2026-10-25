//! AI summaries: a short description and a few tags per item, from
//! Anthropic's Messages API, so a list of items can be skimmed.

use std::{collections::HashMap, env, time::Duration};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;

use crate::item::{self, Item};

const API_VERSION: &str = "2026-10-21";
/// Short descriptions and a few tags are high-volume, simple work: Haiku does
/// them well at a fraction of a larger model's price.
const DEFAULT_MODEL: &str = "claude-haiku-5-5";
/// Items per request.
const BATCH: usize = 25;
/// Room for thinking and 25 descriptions.
const MAX_TOKENS: u32 = 16_000;
/// A batch can take a minute or two; feeds get far less.
const TIMEOUT: Duration = Duration::from_secs(180);
/// Models that accept `fallbacks: "default"`, which reruns a declined
/// request on another model instead of failing it. Haiku has none: a
/// declined batch stays unsummarised, and the next backfill retries it.
const FALLBACK_MODELS: [&str; 4] = [
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
];

const DEFAULT_SYSTEM_PROMPT: &str = "You analyze RSS feed items and return structured data.

For each item in the input JSON array, produce:
- description: 1-2 sentence plain-English summary of what the item is about
- tags: 2-4 high-level tags

Tag guidelines — pick from these categories:
- Format (exactly one): podcast, blog, news, video, newsletter, paper, comic
- Domain (1-2): programming, ai, technology, security, design, business, science, gaming, ...
- Technology (if applicable): typescript, rust, python, javascript, go, linux, ...

Description guidelines:
- This is meant to help someone decide whether the original content is worth opening
- Write plain text, without markdown";

#[derive(Clone)]
pub struct Ai {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    system_prompt: String,
}

impl Ai {
    /// `None` without `ANTHROPIC_API_KEY`: the app works without AI, items
    /// just have no summaries.
    pub fn from_env() -> Option<Self> {
        let api_key = env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())?;
        Some(Self {
            http: crate::http_client::new(TIMEOUT),
            api_key,
            base_url: env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".to_owned()),
            model: env::var("AI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
            system_prompt: env::var("AI_SYSTEM_PROMPT")
                .unwrap_or_else(|_| DEFAULT_SYSTEM_PROMPT.to_owned()),
        })
    }

    /// Summarises the items that have no summary yet, `BATCH` at a time.
    /// Returns how many got one. Stops at the first batch that fails; what
    /// earlier batches saved stays.
    pub async fn enrich(
        &self,
        db: &SqlitePool,
        feed_name: &str,
        items: &[Item],
    ) -> anyhow::Result<usize> {
        let pending: Vec<&Item> = items
            .iter()
            .filter(|item| item.ai_description.is_none())
            .collect();
        let mut done = 0;
        for batch in pending.chunks(BATCH) {
            done += self.enrich_batch(db, feed_name, batch).await?;
        }
        Ok(done)
    }

    async fn enrich_batch(
        &self,
        db: &SqlitePool,
        feed_name: &str,
        batch: &[&Item],
    ) -> anyhow::Result<usize> {
        let inputs: Vec<Input> = batch
            .iter()
            .map(|item| Input {
                id: item.id,
                title: item.title.as_deref().unwrap_or("(no title)"),
                summary: item.summary.as_deref().unwrap_or(""),
                feed: feed_name,
            })
            .collect();
        let (results, usage) = self.ask(&inputs).await?;

        // Only the items that were asked about: an id the model made up
        // could be another feed's item.
        let asked: HashMap<i64, usize> = inputs
            .iter()
            .map(|input| (input.id, json_len(input)))
            .collect();
        let results: Vec<Output> = results
            .into_iter()
            .filter(|r| asked.contains_key(&r.id))
            .collect();
        let input_tokens = split(usage.input_tokens, &asked);
        let output_lengths = results.iter().map(|r| (r.id, json_len(r))).collect();
        let output_tokens = split(usage.output_tokens, &output_lengths);

        for result in &results {
            item::set_enrichment(
                db,
                result.id,
                &result.description,
                &result.tags,
                input_tokens.get(&result.id).copied().unwrap_or(0),
                output_tokens.get(&result.id).copied().unwrap_or(0),
            )
            .await?;
        }
        Ok(results.len())
    }

    /// One Messages API call. The answer's shape is enforced with a JSON
    /// schema, so it parses without coaxing.
    async fn ask(&self, inputs: &[Input<'_>]) -> anyhow::Result<(Vec<Output>, Usage)> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": self.system_prompt,
            "messages": [{ "role": "user", "content": serde_json::to_string(inputs)? }],
            "output_config": {
                // Summaries don't need deep thought, and it keeps them cheap.
                "effort": "low",
                "format": { "type": "json_schema", "schema": answer_schema() },
            },
        });
        let mut request = self
            .http
            .post(format!(
                "{}/v1/messages",
                self.base_url.trim_end_matches('/')
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        if FALLBACK_MODELS.contains(&self.model.as_str()) {
            body["fallbacks"] = json!("default");
            request = request.header("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        let response = request
            .json(&body)
            .send()
            .await
            .context("calling the AI service")?;
        let status = response.status();
        if !status.is_success() {
            let error: serde_json::Value = response.json().await.unwrap_or_default();
            let message = error["error"]["message"].as_str().unwrap_or("no details");
            bail!("the AI service answered {}: {message}", status.as_u16());
        }
        let response: Response = response
            .json()
            .await
            .context("reading the AI service's answer")?;
        match response.stop_reason.as_deref() {
            Some("refusal") => bail!("the model declined to summarise these items"),
            Some("max_tokens") => bail!("the answer was cut off at {MAX_TOKENS} tokens"),
            _ => {}
        }
        // Thinking comes first; the answer is the text after it.
        let text = response
            .content
            .iter()
            .find_map(|block| {
                (block.kind == "text")
                    .then_some(block.text.as_deref())
                    .flatten()
            })
            .context("the answer had no text")?;
        let answer: Answer =
            serde_json::from_str(text).context("the answer wasn't the JSON asked for")?;
        Ok((answer.items, response.usage))
    }
}

fn answer_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "description": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                    },
                    "required": ["id", "description", "tags"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["items"],
        "additionalProperties": false,
    })
}

#[derive(Serialize)]
struct Input<'a> {
    id: i64,
    title: &'a str,
    summary: &'a str,
    feed: &'a str,
}

#[derive(Debug, Deserialize, Serialize)]
struct Output {
    id: i64,
    description: String,
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct Answer {
    items: Vec<Output>,
}

#[derive(Deserialize)]
struct Response {
    content: Vec<Block>,
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    input_tokens: i64,
    output_tokens: i64,
}

fn json_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_string(value).map_or(0, |json| json.len())
}

/// A batch's token count shared out by how much of the batch each item
/// was, so per-item costs add up to roughly what was billed.
fn split(total: i64, lengths: &HashMap<i64, usize>) -> HashMap<i64, i64> {
    let sum: usize = lengths.values().sum();
    lengths
        .iter()
        .map(|(id, len)| {
            let share = if sum > 0 {
                total as f64 * *len as f64 / sum as f64
            } else {
                total as f64 / lengths.len() as f64
            };
            (*id, share.round() as i64)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, http::HeaderMap, routing::post};

    use super::*;
    use crate::{
        feed::{self, tests::settings},
        parse,
    };

    /// A stand-in for the Messages API that answers with `answer` and keeps
    /// what it was sent.
    async fn serve(
        answer: serde_json::Value,
    ) -> (String, Arc<Mutex<Vec<(HeaderMap, serde_json::Value)>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let app = Router::new().route(
            "/v1/messages",
            post(
                move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                    let answer = answer.clone();
                    log.lock().unwrap().push((headers, body));
                    async move { Json(answer) }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), seen)
    }

    fn ai(base_url: &str) -> Ai {
        Ai {
            http: crate::http_client::new(Duration::from_secs(5)),
            api_key: "test-key".to_owned(),
            base_url: base_url.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
        }
    }

    async fn two_items(db: &SqlitePool) -> Vec<Item> {
        let id = feed::create(db, &settings("https://a.example/rss"), 0)
            .await
            .unwrap()
            .unwrap();
        let feed = feed::get(db, id).await.unwrap().unwrap();
        let parsed = ["one", "two"].map(|guid| parse::Item {
            guid: Some(guid.to_owned()),
            title: Some(format!("Title {guid}")),
            ..parse::Item::default()
        });
        item::store_new(db, &feed, &parsed, 0).await.unwrap()
    }

    fn answer(text: &str) -> serde_json::Value {
        json!({
            "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 100, "output_tokens": 40 },
        })
    }

    #[tokio::test]
    async fn saves_each_items_description_tags_and_share_of_tokens() {
        let db = feed::tests::db().await;
        let items = two_items(&db).await;
        let (one, two) = (items[0].id, items[1].id);
        let reply = json!({ "items": [
            { "id": one, "description": "About one.", "tags": ["blog", "rust"] },
            { "id": two, "description": "About two.", "tags": ["news"] },
            { "id": 999, "description": "Made up.", "tags": [] },
        ]});
        let (url, seen) = serve(answer(&reply.to_string())).await;

        let done = ai(&url).enrich(&db, "Example", &items).await.unwrap();
        assert_eq!(done, 2, "the made-up id is ignored");

        let one = item::get(&db, one).await.unwrap().unwrap().item;
        assert_eq!(one.ai_description.as_deref(), Some("About one."));
        assert_eq!(one.tags(), ["blog", "rust"]);
        let two = item::get(&db, two).await.unwrap().unwrap().item;
        let input_tokens = one.ai_input_tokens.unwrap() + two.ai_input_tokens.unwrap();
        assert!((99..=101).contains(&input_tokens), "{input_tokens}");

        let (headers, body) = &seen.lock().unwrap()[0];
        assert_eq!(headers["x-api-key"], "test-key");
        assert_eq!(headers["anthropic-version"], API_VERSION);
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["output_config"]["effort"], "low");
        assert!(body.get("fallbacks").is_none(), "Haiku has no fallback");
        assert!(headers.get("anthropic-beta").is_none());
        let sent: serde_json::Value =
            serde_json::from_str(body["messages"][0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(sent[0]["title"], "Title one");
        assert_eq!(sent[0]["feed"], "Example");
    }

    #[tokio::test]
    async fn asks_models_that_have_one_to_fall_back_when_they_decline() {
        let db = feed::tests::db().await;
        let items = two_items(&db).await;
        let (url, seen) = serve(answer(r#"{"items": []}"#)).await;
        let mut opus = ai(&url);
        opus.model = "claude-opus-5-5".to_owned();
        opus.enrich(&db, "Example", &items).await.unwrap();

        let (headers, body) = &seen.lock().unwrap()[0];
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(headers["anthropic-beta"], "server-side-fallback-2026-07-01");
    }

    #[tokio::test]
    async fn leaves_items_that_have_a_summary_alone() {
        let db = feed::tests::db().await;
        let mut items = two_items(&db).await;
        for item in &mut items {
            item.ai_description = Some("Already done.".to_owned());
        }
        let (url, seen) = serve(answer("{}")).await;
        assert_eq!(ai(&url).enrich(&db, "Example", &items).await.unwrap(), 0);
        assert!(seen.lock().unwrap().is_empty(), "no call was needed");
    }

    #[tokio::test]
    async fn says_why_a_call_failed() {
        let db = feed::tests::db().await;
        let items = two_items(&db).await;

        let mut refused = answer("");
        refused["stop_reason"] = json!("refusal");
        let (url, _) = serve(refused).await;
        let error = ai(&url).enrich(&db, "Example", &items).await.unwrap_err();
        assert!(error.to_string().contains("declined"), "{error}");

        let (url, _) = serve(answer("not json")).await;
        let error = ai(&url).enrich(&db, "Example", &items).await.unwrap_err();
        assert!(error.to_string().contains("wasn't the JSON"), "{error}");
    }

    #[test]
    fn splits_tokens_by_size_and_evenly_when_sizes_are_unknown() {
        let shares = split(90, &HashMap::from([(1, 20), (2, 10)]));
        assert_eq!((shares[&1], shares[&2]), (60, 30));
        let shares = split(10, &HashMap::from([(1, 0), (2, 0)]));
        assert_eq!((shares[&1], shares[&2]), (5, 5));
    }
}
