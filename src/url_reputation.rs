//! Task 1.2 — URL reputation check.
//!
//! Checks a download's origin URL against Google Safe Browsing (API v4,
//! `threatMatches:find`) — the one deliberate second external dependency
//! beyond the AI API already accepted in `PROJECT.md`. Cached locally to
//! stay within the free tier's rate limits and avoid redundant calls.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const SAFE_BROWSING_ENDPOINT: &str = "https://safebrowsing.googleapis.com/v4/threatMatches:find";
const CACHE_TTL: Duration = Duration::from_secs(60 * 30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Safe Browsing matched this URL against a known threat type.
    Malicious(String),
    /// Checked, no match found.
    Clean,
    /// No URL was available to check, or the check itself failed
    /// (network error, etc.) — contributes no signal either way.
    Unknown,
}

#[derive(Serialize)]
struct ThreatMatchesRequest<'a> {
    client: ClientInfo<'a>,
    #[serde(rename = "threatInfo")]
    threat_info: ThreatInfo<'a>,
}

#[derive(Serialize)]
struct ClientInfo<'a> {
    #[serde(rename = "clientId")]
    client_id: &'a str,
    #[serde(rename = "clientVersion")]
    client_version: &'a str,
}

#[derive(Serialize)]
struct ThreatInfo<'a> {
    #[serde(rename = "threatTypes")]
    threat_types: Vec<&'a str>,
    #[serde(rename = "platformTypes")]
    platform_types: Vec<&'a str>,
    #[serde(rename = "threatEntryTypes")]
    threat_entry_types: Vec<&'a str>,
    #[serde(rename = "threatEntries")]
    threat_entries: Vec<ThreatEntry<'a>>,
}

#[derive(Serialize)]
struct ThreatEntry<'a> {
    url: &'a str,
}

#[derive(Deserialize, Default)]
struct ThreatMatchesResponse {
    #[serde(default)]
    matches: Vec<ThreatMatch>,
}

#[derive(Deserialize)]
struct ThreatMatch {
    #[serde(rename = "threatType")]
    threat_type: String,
}

pub struct UrlReputationChecker {
    api_key: String,
    client: reqwest::blocking::Client,
    cache: HashMap<String, (Verdict, Instant)>,
}

impl UrlReputationChecker {
    /// `api_key` should come from an env var / secure config at the call
    /// site — key storage/management itself is an open question, not
    /// solved here (see `OPEN_QUESTIONS.md`-style note in task 1.2).
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::blocking::Client::new(),
            cache: HashMap::new(),
        }
    }

    /// Returns `Unknown` immediately if no URL was resolved by task 1.1 —
    /// this does not block the rest of the pipeline.
    pub fn check(&mut self, url: Option<&str>) -> Verdict {
        let Some(url) = url else {
            return Verdict::Unknown;
        };

        if let Some((verdict, checked_at)) = self.cache.get(url)
            && checked_at.elapsed() < CACHE_TTL
        {
            return verdict.clone();
        }

        let verdict = self.query(url).unwrap_or(Verdict::Unknown);
        self.cache
            .insert(url.to_string(), (verdict.clone(), Instant::now()));
        verdict
    }

    fn query(&self, url: &str) -> anyhow::Result<Verdict> {
        let body = ThreatMatchesRequest {
            client: ClientInfo {
                client_id: "insula",
                client_version: env!("CARGO_PKG_VERSION"),
            },
            threat_info: ThreatInfo {
                threat_types: vec![
                    "MALWARE",
                    "SOCIAL_ENGINEERING",
                    "UNWANTED_SOFTWARE",
                    "POTENTIALLY_HARMFUL_APPLICATION",
                ],
                platform_types: vec!["ANY_PLATFORM"],
                threat_entry_types: vec!["URL"],
                threat_entries: vec![ThreatEntry { url }],
            },
        };

        let resp: ThreatMatchesResponse = self
            .client
            .post(SAFE_BROWSING_ENDPOINT)
            .query(&[("key", self.api_key.as_str())])
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;

        match resp.matches.into_iter().next() {
            Some(m) => Ok(Verdict::Malicious(m.threat_type)),
            None => Ok(Verdict::Clean),
        }
    }
}
