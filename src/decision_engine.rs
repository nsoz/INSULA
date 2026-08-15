//! Task 1.5 — Trigger decision engine.
//!
//! Combines tasks 1.2-1.4/1.6-1.8's signals into the single binary
//! decision `ARCHITECTURE.md` Stage 1 calls for. A rule set, not a
//! tunable score — per Stage 1's original design, the rule stays simple.

use crate::file_type_risk::{FileTypeAssessment, RiskTier};
use crate::local_signature::SignatureTier;
use crate::url_reputation::Verdict as UrlVerdict;

#[derive(Debug, Clone)]
pub struct Decision {
    pub trigger: bool,
    /// Human-readable reason — this is what populates the Stage 2 OS
    /// notification's text once that milestone exists, not just an opaque
    /// yes/no.
    pub reason: String,
}

fn trigger(reason: impl Into<String>) -> Decision {
    Decision {
        trigger: true,
        reason: reason.into(),
    }
}

pub fn decide(
    url_verdict: &UrlVerdict,
    signature: &SignatureTier,
    file_type: &FileTypeAssessment,
    is_symlink: bool,
) -> Decision {
    // Rule 0: a "download" that's actually a symlink is a different kind
    // of concern from the rest of this rule set — it's not about content
    // classification, it's that a genuine browser/app download is never a
    // symlink in the first place, so one appearing at all is already
    // anomalous, independent of what it points at.
    if is_symlink {
        return trigger(
            "This download is a symlink, not a real file — browsers and apps never produce \
             symlink downloads, so this is anomalous regardless of content.",
        );
    }

    // Rule 1: verified external threat intel outranks everything else.
    if let UrlVerdict::Malicious(threat_type) = url_verdict {
        return trigger(format!(
            "The source URL is flagged by Safe Browsing as {threat_type}."
        ));
    }

    // Rule 2: a filename actively lying about itself (bidi override,
    // double extension) is a strong signal about intent, independent of
    // what the bytes turn out to be.
    if let Some(reason) = &file_type.filename_obfuscation {
        return trigger(format!("Filename looks deliberately disguised: {reason}."));
    }

    // Rule 3: a file lying about its own type is a strong signal on its
    // own, regardless of tier.
    if file_type.extension_mismatch {
        return trigger(format!(
            "This file's extension doesn't match its actual content{}.",
            file_type
                .true_type
                .as_ref()
                .map(|t| format!(" (detected: {t})"))
                .unwrap_or_default()
        ));
    }

    // Rule 4: an archive that's carrying something high-risk inside it.
    if let Some(entry) = &file_type.archive_high_risk_entry {
        return trigger(format!(
            "Archive pre-inspection found a risky entry: {entry}."
        ));
    }

    // Rule 5: static string scan turned up known-suspicious patterns.
    if !file_type.suspicious_strings.is_empty() {
        return trigger(format!(
            "Static scan found suspicious content: {}.",
            file_type.suspicious_strings.join("; ")
        ));
    }

    // Rule 6: content that looks packed/encrypted on top of already being
    // a high-risk type.
    if file_type.high_entropy {
        return trigger(format!(
            "This file's content has unusually high entropy ({:.2} bits/byte) for its type — \
             consistent with packed or encrypted content.",
            file_type.entropy.unwrap_or_default()
        ));
    }

    // Rule 7: high-risk type, no trustworthy signature.
    if file_type.tier == RiskTier::High
        && matches!(
            signature,
            SignatureTier::Unsigned | SignatureTier::Rejected | SignatureTier::AdHocSigned
        )
    {
        let detail = match signature {
            SignatureTier::AdHocSigned => {
                " (signed, but only ad-hoc — proves nothing about the publisher)"
            }
            _ => "",
        };
        return trigger(format!(
            "This is a high-risk file type and it isn't signed by a recognized developer{detail}."
        ));
    }

    // Rule 8: risk-bearing type we have no local way to verify at all.
    if matches!(file_type.tier, RiskTier::Medium | RiskTier::High)
        && *signature == SignatureTier::NotApplicable
    {
        return trigger("This file type carries real risk and can't be locally verified.");
    }

    // Rule 9: default — nothing fired.
    Decision {
        trigger: false,
        reason: "No risk signals found.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assessment(tier: RiskTier) -> FileTypeAssessment {
        FileTypeAssessment {
            tier,
            true_type: None,
            extension_mismatch: false,
            filename_obfuscation: None,
            sha256: None,
            entropy: None,
            high_entropy: false,
            suspicious_strings: Vec::new(),
            archive_high_risk_entry: None,
        }
    }

    #[test]
    fn rule0_symlink_always_triggers_even_with_clean_everything_else() {
        let d = decide(
            &UrlVerdict::Clean,
            &SignatureTier::Notarized,
            &assessment(RiskTier::Low),
            true,
        );
        assert!(d.trigger);
        assert!(d.reason.contains("symlink"));
    }

    #[test]
    fn rule0_outranks_rule1_ordering_is_irrelevant_since_both_trigger() {
        // Not a meaningful ranking test on its own (both rules trigger
        // regardless of order), but confirms a symlink pointing at a
        // malicious URL still triggers via rule 0's message, not silently
        // falling through.
        let d = decide(
            &UrlVerdict::Malicious("MALWARE".to_string()),
            &SignatureTier::Notarized,
            &assessment(RiskTier::Low),
            true,
        );
        assert!(d.trigger);
    }

    #[test]
    fn rule1_malicious_url_always_triggers_even_with_clean_everything_else() {
        let d = decide(
            &UrlVerdict::Malicious("MALWARE".to_string()),
            &SignatureTier::Notarized,
            &assessment(RiskTier::Low),
            false,
        );
        assert!(d.trigger);
        assert!(d.reason.contains("MALWARE"));
    }

    #[test]
    fn rule2_filename_obfuscation_triggers_regardless_of_tier() {
        let mut a = assessment(RiskTier::Low);
        a.filename_obfuscation = Some("double extension".to_string());
        let d = decide(&UrlVerdict::Clean, &SignatureTier::Notarized, &a, false);
        assert!(d.trigger);
    }

    #[test]
    fn rule3_extension_mismatch_triggers_regardless_of_tier() {
        let mut a = assessment(RiskTier::Low);
        a.extension_mismatch = true;
        let d = decide(&UrlVerdict::Clean, &SignatureTier::Notarized, &a, false);
        assert!(
            d.trigger,
            "mismatch on a low-tier file should still trigger"
        );
    }

    #[test]
    fn rule4_archive_high_risk_entry_triggers() {
        let mut a = assessment(RiskTier::High);
        a.archive_high_risk_entry = Some("payload.app".to_string());
        let d = decide(
            &UrlVerdict::Unknown,
            &SignatureTier::NotApplicable,
            &a,
            false,
        );
        assert!(d.trigger);
        assert!(d.reason.contains("payload.app"));
    }

    #[test]
    fn rule5_suspicious_strings_trigger() {
        let mut a = assessment(RiskTier::High);
        a.suspicious_strings = vec!["curl | sh — pipe-to-shell".to_string()];
        let d = decide(&UrlVerdict::Unknown, &SignatureTier::Notarized, &a, false);
        assert!(d.trigger);
    }

    #[test]
    fn rule6_high_entropy_triggers() {
        let mut a = assessment(RiskTier::High);
        a.high_entropy = true;
        a.entropy = Some(7.9);
        let d = decide(&UrlVerdict::Unknown, &SignatureTier::Notarized, &a, false);
        assert!(d.trigger);
    }

    #[test]
    fn rule7_high_risk_unsigned_triggers() {
        let d = decide(
            &UrlVerdict::Unknown,
            &SignatureTier::Unsigned,
            &assessment(RiskTier::High),
            false,
        );
        assert!(d.trigger);
    }

    #[test]
    fn rule7_high_risk_adhoc_signed_triggers() {
        let d = decide(
            &UrlVerdict::Unknown,
            &SignatureTier::AdHocSigned,
            &assessment(RiskTier::High),
            false,
        );
        assert!(d.trigger);
        assert!(d.reason.contains("ad-hoc"));
    }

    #[test]
    fn rule7_high_risk_notarized_does_not_trigger_rule7() {
        let d = decide(
            &UrlVerdict::Unknown,
            &SignatureTier::Notarized,
            &assessment(RiskTier::High),
            false,
        );
        assert!(!d.trigger);
    }

    #[test]
    fn rule8_medium_risk_with_no_local_verification_triggers() {
        let d = decide(
            &UrlVerdict::Unknown,
            &SignatureTier::NotApplicable,
            &assessment(RiskTier::Medium),
            false,
        );
        assert!(d.trigger);
    }

    #[test]
    fn rule9_low_risk_clean_everything_does_not_trigger() {
        let d = decide(
            &UrlVerdict::Clean,
            &SignatureTier::NotApplicable,
            &assessment(RiskTier::Low),
            false,
        );
        assert!(!d.trigger);
        assert_eq!(d.reason, "No risk signals found.");
    }

    #[test]
    fn rule1_outranks_rule9_even_when_file_looks_totally_benign() {
        let d = decide(
            &UrlVerdict::Malicious("SOCIAL_ENGINEERING".to_string()),
            &SignatureTier::NotApplicable,
            &assessment(RiskTier::Low),
            false,
        );
        assert!(d.trigger);
    }
}
