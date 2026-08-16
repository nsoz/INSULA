//! Insula — Claude API integration: turns `insula_sensor`'s real, filtered
//! events (see `event_filter.rs`) into a narrative report a person can
//! actually read, instead of a mechanical per-line dump.
//!
//! Raw HTTP via `reqwest` (already a dependency) — Rust has no official
//! Anthropic SDK. Model is `claude-sonnet-5`, not the usual default
//! `claude-opus-5`: this is plain narrative synthesis over an already
//! small, already-filtered event list, not hard reasoning, and Sonnet is
//! roughly half the price for it — a deliberate choice, not an oversight.

use serde_json::Value;
use std::path::Path;

const MODEL: &str = "claude-sonnet-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

const SYSTEM_PROMPT: &str = "\
You are the report engine for a dynamic analysis tool called Insula. \
You'll be given a JSON list of raw process and file events, observed by \
an EndpointSecurity sensor while an app ran inside an isolated VM. Write \
a behavioral report from these events for the user to read — honest, and \
grounded strictly in the real data.\n\n\
Explain every step in plain language. Draw a clear line between observed \
facts (a file being created, a process starting) and your own \
interpretation/inference (why it might have been done) — never present \
something uncertain as certain. This is not a source-code-recovery or \
'decompile' claim, only a behavioral observation writeup.\n\n\
Don't overuse technical jargon, but don't skip important technical \
details either (file paths, pids, exit codes). Write in plain text; \
bullet points are fine, but no markdown heading decoration (###, **) — \
this output gets printed straight to a terminal.";

/// Calls the Claude API to narrate `events` (already filtered to the
/// target app's own process tree), returning the response text split into
/// lines ready for `command_line`'s typewriter display. Returns `Err` with
/// a plain-language reason on any failure (missing key, network error,
/// non-2xx response, refusal, ...) — callers decide how to fall back;
/// this never panics or silently returns an empty report.
///
/// `auto_finding` is `ExplorationMode::Unattended`'s own
/// completion/timeout/hang verdict (`None` for a manual, human-ended run)
/// — a fact already established from sensor data, not something Claude
/// needs to infer. Passed in as context so the narrative accounts for it
/// (e.g. explaining *why* a process looks cut off mid-task) rather than
/// contradicting or re-guessing it.
pub fn narrate(
    events: &[Value],
    target_app_path: &Path,
    auto_finding: Option<String>,
) -> Result<Vec<String>, String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "The ANTHROPIC_API_KEY environment variable isn't set".to_string())?;

    let events_json = serde_json::to_string_pretty(events).unwrap_or_default();
    let finding_note = match &auto_finding {
        Some(finding) => format!("\nAuto-run system's own finding: {finding}\n"),
        None => String::new(),
    };
    let user_content = format!(
        "Target app: {}\nObserved event count: {}\n{finding_note}\n\
         Raw events (JSON, chronological order):\n{events_json}\n\n\
         Write the report based on these events.",
        target_app_path.display(),
        events.len(),
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 4096,
        "system": SYSTEM_PROMPT,
        "messages": [{"role": "user", "content": user_content}],
    });

    let client = reqwest::blocking::Client::new();
    let response = client
        .post(API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Claude API request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().unwrap_or_default();
        return Err(format!("Claude API returned {status}: {text}"));
    }

    let parsed: Value = response
        .json()
        .map_err(|e| format!("Failed to parse the Claude API response: {e}"))?;

    if parsed["stop_reason"] == "refusal" {
        return Err("Claude declined to respond to this request".to_string());
    }

    let text = parsed["content"][0]["text"]
        .as_str()
        .ok_or_else(|| "No text found in the Claude API response".to_string())?;

    Ok(text.lines().map(str::to_string).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_api_key_fails_fast_without_a_network_call() {
        // SAFETY: test-only, single-threaded within this test — no other
        // thread reads/writes ANTHROPIC_API_KEY concurrently.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        let result = narrate(&[], Path::new("/tmp/does-not-matter"), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ANTHROPIC_API_KEY"));
    }
}
