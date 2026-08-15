//! Task 1.4 — File type risk classification.
//!
//! Classifies the downloaded file's *true* type (magic bytes), not just
//! its claimed extension — a file claiming `.pdf` that's actually a
//! Mach-O executable is itself flagged as a mismatch, not silently folded
//! into a tier. Extended by tasks 1.6-1.8 (`static_analysis.rs`) with
//! entropy, suspicious strings, filename obfuscation, archive
//! pre-inspection, and hashing — all static-only, nothing here ever
//! executes the file.

use crate::local_signature::is_mach_o;
use crate::static_analysis;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    High,
    Medium,
    Low,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct FileTypeAssessment {
    pub tier: RiskTier,
    pub true_type: Option<String>,
    pub extension_mismatch: bool,
    pub filename_obfuscation: Option<String>,
    pub sha256: Option<String>,
    pub entropy: Option<f64>,
    pub high_entropy: bool,
    pub suspicious_strings: Vec<String>,
    pub archive_high_risk_entry: Option<String>,
}

pub(crate) const HIGH_RISK_EXTENSIONS: &[&str] = &[
    "app", "pkg", "command", "sh", "dmg", "exe", "scr", "bat", "docm", "xlsm", "pptm",
];
const MEDIUM_RISK_EXTENSIONS: &[&str] = &["zip", "tar", "gz", "tgz", "rar", "7z"];
const JPEG_ALIASES: &[&str] = &["jpg", "jpeg"];

/// Pre-inspects the archive types task 1.8 has a reader for. `rar`/`7z`
/// are still MEDIUM_RISK_EXTENSIONS above (tiered by extension alone) but
/// have no pre-inspection implementation yet — no pure-Rust reader for
/// either was pulled in for v1.
fn inspect_archive_by_extension(path: &Path, ext: &str) -> Option<String> {
    match ext {
        "zip" => static_analysis::inspect_zip_archive(path),
        "tar" => static_analysis::inspect_tar_archive(path, false),
        "gz" | "tgz" => static_analysis::inspect_tar_archive(path, true),
        _ => None,
    }
}

pub fn classify(path: &Path, claimed_extension: Option<&str>) -> FileTypeAssessment {
    let claimed = claimed_extension.map(|e| e.to_lowercase());
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let filename_obfuscation = static_analysis::detect_filename_obfuscation(&filename);
    let sha256 = static_analysis::sha256_of_file(path).ok();

    // Mach-O detection first — this is the case `infer` isn't reliably
    // built for, and it's the one most relevant to this system's threat
    // model (a disguised executable).
    if is_mach_o(path) {
        let mismatch = claimed
            .as_deref()
            .map(|c| c != "app" && !HIGH_RISK_EXTENSIONS.contains(&c))
            .unwrap_or(true);
        let (entropy, high_entropy, suspicious_strings) = deep_scan(path, RiskTier::High);
        return FileTypeAssessment {
            tier: RiskTier::High,
            true_type: Some("application/x-mach-binary".to_string()),
            extension_mismatch: mismatch,
            filename_obfuscation,
            sha256,
            entropy,
            high_entropy,
            suspicious_strings,
            archive_high_risk_entry: None,
        };
    }

    let sniffed = infer::get_from_path(path).ok().flatten();
    let true_extension = sniffed.map(|k| k.extension().to_string());
    let true_mime = sniffed.map(|k| k.mime_type().to_string());

    let extension_mismatch = match (&claimed, &true_extension) {
        (Some(c), Some(t)) => c != t && !is_known_alias(c, t),
        _ => false,
    };

    let effective_ext = true_extension.or(claimed);
    let mut tier = effective_ext
        .as_deref()
        .map(tier_for_extension)
        .unwrap_or(RiskTier::Unknown);

    // Archive pre-inspection (task 1.8) — peek at the listing without
    // extracting; a high-risk entry inside upgrades the tier.
    let archive_high_risk_entry = effective_ext
        .as_deref()
        .and_then(|e| inspect_archive_by_extension(path, e));
    if archive_high_risk_entry.is_some() {
        tier = RiskTier::High;
    }

    let (entropy, high_entropy, suspicious_strings) = deep_scan(path, tier);

    FileTypeAssessment {
        tier,
        true_type: true_mime,
        extension_mismatch,
        filename_obfuscation,
        sha256,
        entropy,
        high_entropy,
        suspicious_strings,
        archive_high_risk_entry,
    }
}

/// Runs entropy + suspicious-string scanning (tasks 1.6), gated to
/// Medium/High tier files only — see `static_analysis.rs` for why entropy
/// alone on a Low-tier file isn't a meaningful signal.
fn deep_scan(path: &Path, tier: RiskTier) -> (Option<f64>, bool, Vec<String>) {
    if !matches!(tier, RiskTier::Medium | RiskTier::High) {
        return (None, false, Vec::new());
    }
    let Ok(bytes) = static_analysis::read_capped(path) else {
        return (None, false, Vec::new());
    };
    let entropy = static_analysis::shannon_entropy(&bytes);
    let high_entropy = tier == RiskTier::High && entropy > static_analysis::HIGH_ENTROPY_THRESHOLD;
    let suspicious_strings = static_analysis::scan_suspicious_strings(&bytes);
    (Some(entropy), high_entropy, suspicious_strings)
}

fn tier_for_extension(ext: &str) -> RiskTier {
    let ext = ext.to_lowercase();
    if HIGH_RISK_EXTENSIONS.contains(&ext.as_str()) {
        RiskTier::High
    } else if MEDIUM_RISK_EXTENSIONS.contains(&ext.as_str()) {
        RiskTier::Medium
    } else {
        RiskTier::Low
    }
}

fn is_known_alias(a: &str, b: &str) -> bool {
    JPEG_ALIASES.contains(&a) && JPEG_ALIASES.contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("insula-test-{name}"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn tier_for_extension_covers_all_three_tiers() {
        assert_eq!(tier_for_extension("exe"), RiskTier::High);
        assert_eq!(tier_for_extension("zip"), RiskTier::Medium);
        assert_eq!(tier_for_extension("txt"), RiskTier::Low);
    }

    #[test]
    fn jpg_and_jpeg_are_not_flagged_as_a_mismatch() {
        assert!(is_known_alias("jpg", "jpeg"));
        assert!(is_known_alias("jpeg", "jpg"));
        assert!(!is_known_alias("jpg", "png"));
    }

    #[test]
    fn mach_o_disguised_with_a_text_extension_is_high_risk_and_flagged() {
        // MH_MAGIC_64
        let path = temp_file("fake-invoice.pdf", &[0xfe, 0xed, 0xfa, 0xcf, 0, 0, 0, 0]);
        let assessment = classify(&path, Some("pdf"));
        assert_eq!(assessment.tier, RiskTier::High);
        assert!(assessment.extension_mismatch);
        assert!(assessment.sha256.is_some());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn plain_text_with_matching_extension_is_low_risk_no_mismatch() {
        let path = temp_file("note.txt", b"just a normal text file");
        let assessment = classify(&path, Some("txt"));
        assert_eq!(assessment.tier, RiskTier::Low);
        assert!(!assessment.extension_mismatch);
        // Low tier -> deep_scan should not have run.
        assert!(assessment.entropy.is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rtl_disguised_filename_is_flagged_even_with_matching_bytes() {
        let path = temp_file("note\u{202E}txt.exe", &[0xfe, 0xed, 0xfa, 0xcf, 0, 0, 0, 0]);
        let assessment = classify(&path, Some("exe"));
        assert!(assessment.filename_obfuscation.is_some());
        std::fs::remove_file(path).ok();
    }
}
