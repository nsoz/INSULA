//! Task 1.3 — Local signature check.
//!
//! Asks macOS's own Gatekeeper what it already thinks of this file — zero
//! network calls, zero new external dependency, since it's the OS's own
//! local decision. Shells out to `spctl`/`codesign` (the pragmatic v1
//! default over calling the Security framework directly).

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureTier {
    /// Apple-verified: notarized, trusted developer ID.
    Notarized,
    /// Has a valid signature but hasn't passed notarization.
    SignedNotNotarized,
    /// Signed, but only ad-hoc — a signature anyone can generate locally
    /// in seconds, proving nothing about the publisher's identity. A
    /// known evasion pattern: sign ad-hoc just to get past a naive "is it
    /// signed at all" check (task 1.8).
    AdHocSigned,
    Unsigned,
    /// Actively blocked by current Gatekeeper policy.
    Rejected,
    /// Not an executable/app-bundle type this check means anything for.
    NotApplicable,
}

pub fn check(path: &Path) -> SignatureTier {
    if !is_executable_type(path) {
        return SignatureTier::NotApplicable;
    }

    let spctl_output = Command::new("spctl")
        .args(["--assess", "--type", "execute", "-v"])
        .arg(path)
        .output();
    let spctl_ok = matches!(&spctl_output, Ok(o) if o.status.success());
    let spctl_stderr = spctl_output
        .as_ref()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .unwrap_or_default();

    let codesign_output = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(path)
        .output();
    let is_signed = matches!(&codesign_output, Ok(o) if o.status.success());
    let codesign_stderr = codesign_output
        .as_ref()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .unwrap_or_default();

    if !is_signed {
        return SignatureTier::Unsigned;
    }
    if codesign_stderr.contains("Signature=adhoc") {
        return SignatureTier::AdHocSigned;
    }
    if !spctl_ok {
        return SignatureTier::Rejected;
    }
    if spctl_stderr.to_lowercase().contains("notarized") {
        SignatureTier::Notarized
    } else {
        SignatureTier::SignedNotNotarized
    }
}

fn is_executable_type(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "app" | "pkg" | "command" | "sh" | "dmg") || is_mach_o(path)
}

/// Checks Mach-O magic bytes directly rather than relying on extension —
/// this is what catches a binary disguised with a non-executable
/// extension (the `invoice.pdf` scenario from the milestone walkthrough).
pub fn is_mach_o(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 4];
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    const MACHO_MAGICS: [[u8; 4]; 4] = [
        [0xfe, 0xed, 0xfa, 0xce], // MH_MAGIC (32-bit)
        [0xfe, 0xed, 0xfa, 0xcf], // MH_MAGIC_64
        [0xce, 0xfa, 0xed, 0xfe], // MH_CIGAM (byte-swapped 32-bit)
        [0xcf, 0xfa, 0xed, 0xfe], // MH_CIGAM_64
    ];
    MACHO_MAGICS.contains(&buf) || buf == [0xca, 0xfe, 0xba, 0xbe] // fat binary
}
