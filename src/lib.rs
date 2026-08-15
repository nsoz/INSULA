pub mod app_target;
pub mod cli;
pub mod decision_engine;
pub mod download_detection;
pub mod file_type_risk;
pub mod local_signature;
pub mod static_analysis;
pub mod url_reputation;
pub mod vm;

use std::path::PathBuf;
use std::time::SystemTime;

/// Output of Task 1.1 — a confirmed, genuine download event, ready to be
/// evaluated by tasks 1.2-1.4 and combined by task 1.5.
#[derive(Debug, Clone)]
pub struct DownloadEvent {
    pub path: PathBuf,
    pub filename: String,
    pub claimed_extension: Option<String>,
    pub quarantine_confirmed: bool,
    pub origin_url: Option<String>,
    pub timestamp: SystemTime,
    pub source_app: Option<String>,
    /// A genuine browser/app download is never a symlink — one landing in
    /// `Downloads` is inherently anomalous. See `download_detection.rs`.
    pub is_symlink: bool,
}
