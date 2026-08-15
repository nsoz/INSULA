//! Task 1.1 — Download detection.
//!
//! Watches `~/Downloads`, confirms genuine quarantine-flagged download
//! events (ignoring in-progress temp files), resolves the origin URL from
//! macOS's own LaunchServices quarantine events database, and immediately
//! locks the file before handing it off to the rest of the pipeline.

use crate::DownloadEvent;
use anyhow::{Context, Result};
use notify::{Event, RecursiveMode, Watcher};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::SystemTime;

const TEMP_EXTENSIONS: &[&str] = &["crdownload", "download", "part", "partial", "tmp"];
const QUARANTINE_XATTR: &str = "com.apple.quarantine";

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable not set")
}

/// Holding area new downloads get locked into until the rest of the
/// pipeline (tasks 1.2-1.5, and later milestones) clears them.
fn holding_dir() -> Result<PathBuf> {
    let dir = home_dir()?.join(".insula").join("holding");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Watches `~/Downloads` and yields a `DownloadEvent` for every genuine,
/// completed, quarantine-flagged download. Blocks the calling thread —
/// intended to run on its own thread/process (the eventual LaunchAgent).
pub fn watch_downloads<F>(mut on_event: F) -> Result<()>
where
    F: FnMut(DownloadEvent),
{
    let downloads = home_dir()?.join("Downloads");
    let (tx, rx) = channel::<notify::Result<Event>>();

    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&downloads, RecursiveMode::NonRecursive)?;

    for res in rx {
        let event = match res {
            Ok(e) => e,
            Err(e) => {
                eprintln!("insula: watch error: {e}");
                continue;
            }
        };

        for path in event.paths {
            if is_temp_download_name(&path) {
                continue;
            }

            // Use symlink-aware metadata (doesn't follow the link) rather
            // than `Path::is_file()`, which *does* follow symlinks and
            // would silently treat a symlink download as a normal file.
            //
            // No separate "already seen this path" bookkeeping is kept —
            // deliberately. An earlier version tracked processed paths in
            // a `HashSet` that never forgot them, which meant a *second*,
            // later download that happened to reuse the same filename was
            // silently ignored forever (confirmed live: a second
            // `report.txt` never got evaluated at all). This
            // existence check is what duplicate-event suppression
            // actually needs: once a file's been locked away, this path
            // legitimately doesn't exist anymore, so a redundant FSEvents
            // notification for it fails here and is skipped naturally —
            // without permanently blocking a genuinely new file that
            // later reuses the name.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let is_link = meta.file_type().is_symlink();
            if !is_link && !meta.is_file() {
                continue;
            }

            let Some(mut download_event) = evaluate_candidate(&path, is_link) else {
                continue;
            };

            match lock_file(&download_event.path, is_link) {
                Ok(new_path) => {
                    download_event.path = new_path;
                    on_event(download_event);
                }
                Err(e) => eprintln!("insula: failed to lock {}: {e}", path.display()),
            }
        }
    }

    Ok(())
}

fn is_temp_download_name(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEMP_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Confirms the quarantine xattr is present and builds the event record.
/// Returns `None` for files that aren't genuine quarantine-flagged
/// downloads — the known v1 limitation: curl/wget-style downloads that
/// never set the xattr are invisible here, by design for now.
///
/// Symlinks are the one exception: a genuine browser/app download is
/// never a symlink, so one landing in `~/Downloads` is inherently
/// anomalous regardless of quarantine status — it's still built into a
/// `DownloadEvent` (with `is_symlink: true`) even without the xattr,
/// rather than silently skipped.
fn evaluate_candidate(path: &Path, is_link: bool) -> Option<DownloadEvent> {
    let xattr_raw = xattr::get(path, QUARANTINE_XATTR).ok().flatten();
    if xattr_raw.is_none() && !is_link {
        return None;
    }

    let (source_app, event_uuid) = match &xattr_raw {
        Some(raw) => {
            let raw = String::from_utf8_lossy(raw);
            // Format: flags;timestamp_hex;agent_name;event_uuid
            let parts: Vec<&str> = raw.split(';').collect();
            (
                parts.get(2).map(|s| s.to_string()),
                parts.get(3).map(|s| s.to_string()),
            )
        }
        None => (None, None),
    };

    let origin_url = event_uuid
        .as_deref()
        .and_then(|uuid| lookup_origin_url(uuid).ok().flatten());

    Some(DownloadEvent {
        path: path.to_path_buf(),
        filename: path.file_name()?.to_string_lossy().to_string(),
        claimed_extension: path.extension().map(|e| e.to_string_lossy().to_string()),
        quarantine_confirmed: xattr_raw.is_some(),
        origin_url,
        timestamp: SystemTime::now(),
        source_app,
        is_symlink: is_link,
    })
}

/// Resolves an origin URL from macOS's own LaunchServices quarantine
/// events database, by event UUID. Schema confirmed empirically against
/// `~/Library/Preferences/com.apple.LaunchServices.QuarantineEventsV2`
/// (SQLite, table `LSQuarantineEvent`) — not all events carry a URL (e.g.
/// AirDrop, Homebrew installs leave this empty), so `None` is a normal,
/// expected outcome, not a failure.
fn lookup_origin_url(event_uuid: &str) -> Result<Option<String>> {
    let db_path =
        home_dir()?.join("Library/Preferences/com.apple.LaunchServices.QuarantineEventsV2");
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT LSQuarantineDataURLString, LSQuarantineOriginURLString \
         FROM LSQuarantineEvent WHERE LSQuarantineEventIdentifier = ?1",
    )?;
    let mut rows = stmt.query([event_uuid])?;
    if let Some(row) = rows.next()? {
        let data_url: Option<String> = row.get(0)?;
        let origin_url: Option<String> = row.get(1)?;
        return Ok(data_url
            .filter(|s| !s.is_empty())
            .or_else(|| origin_url.filter(|s| !s.is_empty())));
    }
    Ok(None)
}

/// The actual guarantee this task provides: the file becomes physically
/// inert the instant it's confirmed as a genuine download, before tasks
/// 1.2-1.5 (which may take time, especially the networked Safe Browsing
/// check) ever run. Strips execute permission and relocates the file out
/// of its expected path so nothing can open it by its original name.
/// Returns the new path. Released only by downstream logic (task 1.5
/// clearing it, the user declining the Stage 2 notification, or a clean
/// Stage 9 verdict) — not implemented here.
///
/// For symlinks, permission-stripping is skipped entirely: `std::fs`'s
/// metadata/permissions calls follow symlinks by default, so chmod-ing a
/// symlink would silently mutate whatever real file it points at —
/// potentially something completely outside `Downloads` and outside this
/// system's remit. Renaming a symlink is still safe; it moves the link
/// itself, never touches its target.
fn lock_file(path: &Path, is_symlink: bool) -> Result<PathBuf> {
    if !is_symlink {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() & !0o111); // strip all execute bits
        std::fs::set_permissions(path, perms)?;
    }

    let holding = holding_dir()?;
    let filename = path.file_name().context("path has no filename")?;
    let dest = unique_holding_path(&holding, filename);
    std::fs::rename(path, &dest)?;

    Ok(dest)
}

/// Avoids silently clobbering a previously-locked file of the same name —
/// two downloads with an identical filename (from different sources, on
/// different days) would otherwise overwrite each other in the holding
/// directory, quietly losing whichever landed first.
fn unique_holding_path(holding: &Path, filename: &std::ffi::OsStr) -> PathBuf {
    let candidate = holding.join(filename);
    if !candidate.exists() {
        return candidate;
    }

    let name_path = Path::new(filename);
    let stem = name_path
        .file_stem()
        .unwrap_or(filename)
        .to_string_lossy()
        .to_string();
    let ext = name_path
        .extension()
        .map(|e| e.to_string_lossy().to_string());

    for i in 1..100_000 {
        let candidate_name = match &ext {
            Some(e) => format!("{stem}-{i}.{e}"),
            None => format!("{stem}-{i}"),
        };
        let candidate = holding.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    // Exhausted a very large search space — fall back to a timestamp
    // suffix, which is astronomically unlikely to collide.
    let fallback = format!(
        "{stem}-{}",
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    holding.join(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_holding_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("insula-test-holding-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unique_holding_path_returns_plain_name_when_nothing_collides() {
        let dir = temp_holding_dir("empty");
        let path = unique_holding_path(&dir, std::ffi::OsStr::new("report.txt"));
        assert_eq!(path, dir.join("report.txt"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unique_holding_path_avoids_clobbering_an_existing_file() {
        // This is the regression test for the exact bug found live: a
        // second download reusing an earlier download's filename must not
        // silently overwrite it.
        let dir = temp_holding_dir("collision");
        std::fs::write(dir.join("report.txt"), b"first version").unwrap();

        let path = unique_holding_path(&dir, std::ffi::OsStr::new("report.txt"));
        assert_ne!(
            path,
            dir.join("report.txt"),
            "must not reuse the colliding name"
        );
        assert_eq!(path, dir.join("report-1.txt"));

        std::fs::write(&path, b"second version").unwrap();
        assert_eq!(
            std::fs::read(dir.join("report.txt")).unwrap(),
            b"first version",
            "the original file must be untouched"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unique_holding_path_finds_the_next_free_slot_across_multiple_collisions() {
        let dir = temp_holding_dir("multi-collision");
        std::fs::write(dir.join("note.txt"), b"a").unwrap();
        std::fs::write(dir.join("note-1.txt"), b"b").unwrap();
        std::fs::write(dir.join("note-2.txt"), b"c").unwrap();

        let path = unique_holding_path(&dir, std::ffi::OsStr::new("note.txt"));
        assert_eq!(path, dir.join("note-3.txt"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unique_holding_path_handles_extensionless_filenames() {
        let dir = temp_holding_dir("no-ext");
        std::fs::write(dir.join("payload"), b"x").unwrap();
        let path = unique_holding_path(&dir, std::ffi::OsStr::new("payload"));
        assert_eq!(path, dir.join("payload-1"));
        std::fs::remove_dir_all(dir).ok();
    }
}
