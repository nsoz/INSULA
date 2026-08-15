//! Insula — Milestone 1: detection engine.
//!
//! Runs tasks 1.1-1.5 end to end: watches `~/Downloads`, confirms genuine
//! downloads, locks them immediately, checks URL reputation / local
//! signature / file type, and prints the trigger decision. Milestone 2
//! (the OS notification this decision should feed into) doesn't exist
//! yet — this binary is the detection engine on its own, verifiable in
//! isolation.

use insula::url_reputation::UrlReputationChecker;
use insula::{decision_engine, download_detection, file_type_risk, local_signature};

fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("INSULA_SAFE_BROWSING_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!(
            "insula: warning — INSULA_SAFE_BROWSING_API_KEY not set; \
             URL reputation checks will report Unknown for every download"
        );
    }
    let mut url_checker = UrlReputationChecker::new(api_key);

    println!("insula: watching ~/Downloads (Milestone 1 — detection engine)");

    download_detection::watch_downloads(|event| {
        println!("\n--- download detected: {} ---", event.filename);
        println!("  path:        {}", event.path.display());
        println!("  origin url:  {:?}", event.origin_url);
        println!("  source app:  {:?}", event.source_app);
        println!("  is symlink:  {}", event.is_symlink);

        let url_verdict = url_checker.check(event.origin_url.as_deref());
        let signature = local_signature::check(&event.path);
        let file_type = file_type_risk::classify(&event.path, event.claimed_extension.as_deref());

        println!("  safe browsing: {url_verdict:?}");
        println!("  signature:     {signature:?}");
        println!("  tier:          {:?}", file_type.tier);
        println!("  true type:     {:?}", file_type.true_type);
        println!("  sha256:        {:?}", file_type.sha256);
        println!("  ext mismatch:  {}", file_type.extension_mismatch);
        println!("  filename trick:{:?}", file_type.filename_obfuscation);
        println!(
            "  entropy:       {:?} (high={})",
            file_type.entropy, file_type.high_entropy
        );
        println!("  suspicious:    {:?}", file_type.suspicious_strings);
        println!("  archive risk:  {:?}", file_type.archive_high_risk_entry);

        let decision =
            decision_engine::decide(&url_verdict, &signature, &file_type, event.is_symlink);
        println!(
            "  DECISION: trigger={} reason=\"{}\"",
            decision.trigger, decision.reason
        );
        println!("  (Milestone 2 — OS notification — not built yet; hand-off point)");
    })
}
