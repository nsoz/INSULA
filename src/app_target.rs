//! App-submission target validation (`ARCHITECTURE.md` Stage 1,
//! `OPEN_QUESTIONS.md` §8): before Insula ever asks how exploration should
//! happen, the path the user hands it has to actually point at something
//! Insula's VM pipeline could run. Two checks, in order: does the path
//! exist at all, and is whatever's there actually runnable on this host
//! OS — a `.app` bundle or a real executable, not an arbitrary file.
//!
//! Host-OS-adaptive, same reasoning as the VM backend/guest selection
//! (`ROADMAP.md`): what counts as "runnable" differs by platform, and
//! Windows hosts are out of scope for v1 entirely.
//!
//! **Read-only**, same invariant Milestone 1's static analysis holds:
//! nothing here ever opens, executes, or modifies the target beyond
//! reading its metadata/directory listing/magic bytes.

use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", target_os = "linux"))]
const HOST_OS_SUPPORTED: bool = true;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const HOST_OS_SUPPORTED: bool = false;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTargetError {
    NotFound,
    NotRunnable,
    UnsupportedHostOs,
}

impl AppTargetError {
    /// Plain-language reason, meant to be surfaced directly in the
    /// onboarding prompt.
    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => "Bu yolda bir dosya veya uygulama bulunamadı.",
            Self::NotRunnable => {
                "Bu, bu işletim sisteminde çalıştırılabilir bir uygulama gibi görünmüyor."
            }
            Self::UnsupportedHostOs => {
                "Bu işletim sistemi için henüz destek yok (v1: yalnızca macOS ve Linux)."
            }
        }
    }
}

/// Expands a leading `~` to the user's home directory — typed paths and
/// most terminal drag-and-drop insertions use this shorthand. Anything
/// else is passed through unchanged. Doesn't canonicalize or resolve
/// symlinks — what's validated should still look like what the user typed.
pub fn expand(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    } else if trimmed == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(trimmed)
}

/// Validates that `path` both exists and points at something runnable on
/// the current host OS.
pub fn validate(path: &Path) -> Result<(), AppTargetError> {
    if !HOST_OS_SUPPORTED {
        return Err(AppTargetError::UnsupportedHostOs);
    }
    if !path.exists() {
        return Err(AppTargetError::NotFound);
    }
    if is_runnable(path) {
        Ok(())
    } else {
        Err(AppTargetError::NotRunnable)
    }
}

#[cfg(target_os = "macos")]
fn is_runnable(path: &Path) -> bool {
    if path.is_dir() {
        return path.extension().is_some_and(|ext| ext == "app") && app_bundle_has_executable(path);
    }
    is_executable_file(path)
}

#[cfg(target_os = "macos")]
fn app_bundle_has_executable(bundle: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(bundle.join("Contents").join("MacOS")) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| entry.path().is_file() && is_executable_file(&entry.path()))
}

#[cfg(target_os = "linux")]
fn is_runnable(path: &Path) -> bool {
    path.is_file() && is_executable_file(path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn is_runnable(_path: &Path) -> bool {
    false
}

/// A file counts as runnable if the OS's own executable permission bit is
/// set, or it's a real native binary by magic bytes (Mach-O on macOS, ELF
/// on Linux). Permission bit alone also passes a `+x` shell script, which
/// genuinely is runnable even though it isn't a compiled binary.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let has_exec_bit = std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    has_exec_bit || has_known_binary_magic(path)
}

#[cfg(target_os = "macos")]
fn has_known_binary_magic(path: &Path) -> bool {
    crate::local_signature::is_mach_o(path)
}

#[cfg(target_os = "linux")]
fn has_known_binary_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).is_ok() && buf == [0x7f, b'E', b'L', b'F']
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("insula-test-apptarget-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn missing_path_is_not_found() {
        let path = std::env::temp_dir().join("insula-test-apptarget-does-not-exist");
        assert_eq!(validate(&path), Err(AppTargetError::NotFound));
    }

    #[test]
    fn plain_non_executable_file_is_rejected() {
        let dir = temp_dir("plain-file");
        let path = dir.join("notes.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"just text")
            .unwrap();
        assert_eq!(validate(&path), Err(AppTargetError::NotRunnable));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn executable_permission_bit_alone_counts_as_runnable() {
        let dir = temp_dir("exec-script");
        let path = dir.join("run.sh");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"#!/bin/sh\necho hi\n")
            .unwrap();
        make_executable(&path);
        assert_eq!(validate(&path), Ok(()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mach_o_without_exec_bit_still_counts_as_runnable() {
        let dir = temp_dir("mach-o-no-bit");
        let path = dir.join("binary");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&[0xfe, 0xed, 0xfa, 0xcf, 0, 0, 0, 0])
            .unwrap();
        assert_eq!(validate(&path), Ok(()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn app_bundle_without_macos_executable_is_rejected() {
        let dir = temp_dir("empty-bundle");
        let bundle = dir.join("Empty.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        assert_eq!(validate(&bundle), Err(AppTargetError::NotRunnable));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn app_bundle_with_executable_is_accepted() {
        let dir = temp_dir("real-bundle");
        let bundle = dir.join("Real.app");
        let macos_dir = bundle.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();
        let exe = macos_dir.join("Real");
        std::fs::File::create(&exe)
            .unwrap()
            .write_all(&[0xfe, 0xed, 0xfa, 0xcf, 0, 0, 0, 0])
            .unwrap();
        assert_eq!(validate(&bundle), Ok(()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tilde_prefix_expands_to_home() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            expand("~/Downloads/App.app"),
            Path::new(&home).join("Downloads/App.app")
        );
        assert_eq!(expand("~"), PathBuf::from(&home));
        assert_eq!(expand("/tmp/App.app"), PathBuf::from("/tmp/App.app"));
    }
}
