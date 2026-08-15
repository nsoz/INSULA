//! Insula — Milestone 3: launching the analysis VM and transferring the
//! submitted app into it.
//!
//! Wraps `tart run <name> --no-graphics --vnc-experimental` as a child
//! process (`ARCHITECTURE.md` Stage 3). Tart owns the `VZVirtualMachine`
//! it creates, not Insula directly — see the `project-insula-vm-tooling`
//! memory for why that ownership is exactly what makes input-injection
//! possible at all (confirmed against UTM's Apple Virtualization backend,
//! which does *not* expose this because UTM's own process is the owner
//! there instead).
//!
//! Every real analysis clones a fresh, disposable VM from the *golden*
//! image (`insula_setup`'s prepared `insula-macos` — SIP already
//! disabled, the ESF sensor already installed) rather than running the
//! golden image directly: the golden image has to stay pristine and
//! reusable indefinitely, and `PROJECT.md`'s safety guarantee is that
//! nothing persists between analysis runs, which only holds if each run
//! gets a genuinely fresh instance. Tart's clone is APFS-clonefile-based
//! (near-instant, cheap copy-on-write), so this costs a run essentially
//! nothing over running the golden image directly.
//!
//! Tart prints a `VNC server is running at vnc://...` line to stdout once
//! the VM has booted and its VNC server is ready — booting a macOS guest
//! takes tens of seconds, so that line is read on a background thread and
//! handed back over a channel rather than blocking the caller.
//!
//! The submitted app is delivered via Tart's `--dir` directory-share
//! (VirtioFS), mounted read-only, rather than sharing the app's own
//! parent folder — sharing e.g. `~/Downloads` wholesale would expose
//! everything else sitting next to it into the guest, not just the one
//! app the user actually submitted. So the app is first copied into an
//! isolated, per-run staging directory that contains nothing else, and
//! *that* directory is what gets shared.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const VNC_READY_PREFIX: &str = "VNC server is running at ";

/// Mount tag for the shared directory — inside the guest this shows up
/// under `/Volumes/My Shared Files/insula-app`.
const SHARE_TAG: &str = "insula-app";

pub struct RunningVm {
    child: Child,
    vnc_ready: Receiver<String>,
    staging_dir: PathBuf,
    /// Name of the disposable per-run clone — never the golden image
    /// itself — deleted in `Drop` once this run is over.
    ephemeral_vm_name: String,
}

impl RunningVm {
    /// Clones `golden_vm_name` (the `insula_setup`-prepared image) into a
    /// fresh, uniquely-named disposable instance, then spawns `tart run
    /// <clone> --no-graphics --vnc-experimental --dir insula-app:<staging>:ro`
    /// against that clone — after first copying `app_path` into a fresh
    /// staging directory. Fails immediately if cloning, staging the app,
    /// or spawning `tart` itself fails — does not wait for the VM to
    /// actually finish booting.
    ///
    /// `auto_open_vnc` should be `true` only for manual exploration, where
    /// the user needs to actually see and drive the VM's screen
    /// themselves once it's ready — for Claude-driven exploration, Claude
    /// is the one connecting over VNC, so there's no reason to also pop
    /// open a visible Screen Sharing window for the user.
    pub fn launch(
        golden_vm_name: &str,
        app_path: &Path,
        auto_open_vnc: bool,
    ) -> std::io::Result<Self> {
        let staging_dir = stage_app_for_transfer(app_path)?;
        let dir_arg = format!("{SHARE_TAG}:{}:ro", staging_dir.display());

        let ephemeral_vm_name = format!(
            "insula-run-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );

        let clone_status = Command::new("tart")
            .args(["clone", golden_vm_name, &ephemeral_vm_name])
            .status()?;
        if !clone_status.success() {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(std::io::Error::other(format!(
                "tart clone {golden_vm_name} -> {ephemeral_vm_name} failed"
            )));
        }

        let mut child = Command::new("tart")
            .args([
                "run",
                &ephemeral_vm_name,
                "--no-graphics",
                "--vnc-experimental",
                "--dir",
                &dir_arg,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout was piped above");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = line.strip_prefix(VNC_READY_PREFIX) {
                    // The CLI can only ever show this URL (password
                    // included) as plain TUI text, which isn't
                    // selectable/copyable in a raw-mode alternate screen —
                    // opening it directly is what actually gets the user
                    // connected. macOS's built-in Screen Sharing app
                    // understands `vnc://` URLs natively, so no VNC client
                    // needs to be installed for this to work. Only done
                    // for manual exploration — see `launch`'s docs.
                    if auto_open_vnc {
                        let _ = Command::new("open").arg(url).spawn();
                    }
                    let _ = tx.send(url.to_string());
                    return;
                }
            }
        });

        Ok(Self {
            child,
            vnc_ready: rx,
            staging_dir,
            ephemeral_vm_name,
        })
    }

    /// Non-blocking check for the VNC connection URL. Returns `None`
    /// until Tart has actually printed it (the VM is still booting) —
    /// call again on a later tick rather than blocking on it.
    pub fn poll_vnc_url(&self) -> Option<String> {
        self.vnc_ready.try_recv().ok()
    }
}

impl Drop for RunningVm {
    /// Never leave an analysis VM running, its disposable clone sitting
    /// around, or its staged copy of the submitted app on disk, once
    /// Insula stops watching it — matches `PROJECT.md`'s safety guarantee
    /// that nothing persists between runs. `tart stop` before `delete` is
    /// belt-and-suspenders: `child.kill()` should already tear the VM
    /// down immediately (its `VZVirtualMachine` lives in that process),
    /// but giving Tart an explicit stop first avoids a `delete` racing a
    /// not-quite-finished teardown.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.staging_dir);
        let _ = Command::new("tart")
            .args(["stop", &self.ephemeral_vm_name])
            .status();
        let _ = Command::new("tart")
            .args(["delete", &self.ephemeral_vm_name])
            .status();
    }
}

/// Copies `app_path` (a `.app` bundle directory or a standalone
/// executable file) into a fresh, empty staging directory under the
/// system temp dir, and returns that staging directory's path — never the
/// original path itself, so the directory handed to `--dir` never
/// contains anything the user didn't submit.
fn stage_app_for_transfer(app_path: &Path) -> std::io::Result<PathBuf> {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let staging_dir = std::env::temp_dir().join("insula-staging").join(unique);
    std::fs::create_dir_all(&staging_dir)?;

    let file_name = app_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "submitted path has no file name",
        )
    })?;
    let destination = staging_dir.join(file_name);

    if app_path.is_dir() {
        copy_dir_recursive(app_path, &destination)?;
    } else {
        std::fs::copy(app_path, &destination)?;
    }

    Ok(staging_dir)
}

/// Recursive copy for `.app` bundles (plain directories of files —
/// nothing here needs to understand bundle structure). Symlinks are
/// recreated as symlinks rather than followed/flattened, so staging never
/// silently pulls in content from outside the original bundle.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());

        if file_type.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &dst_path)?;
        } else if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("insula-test-vm-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn staging_a_plain_file_copies_it_by_name_into_an_isolated_dir() {
        let source_dir = temp_dir("plain-file-source");
        let app_path = source_dir.join("MyTool");
        std::fs::File::create(&app_path)
            .unwrap()
            .write_all(b"not a real binary")
            .unwrap();

        let staging_dir = stage_app_for_transfer(&app_path).unwrap();
        let staged_copy = staging_dir.join("MyTool");

        assert!(staged_copy.is_file());
        assert_eq!(std::fs::read(&staged_copy).unwrap(), b"not a real binary");
        // The staging dir must contain only what was submitted, not
        // anything else from the source directory it came from.
        assert_eq!(std::fs::read_dir(&staging_dir).unwrap().count(), 1);

        std::fs::remove_dir_all(&source_dir).ok();
        std::fs::remove_dir_all(&staging_dir).ok();
    }

    #[test]
    fn staging_an_app_bundle_copies_it_recursively() {
        let source_dir = temp_dir("bundle-source");
        let bundle = source_dir.join("Real.app");
        let macos_dir = bundle.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();
        std::fs::File::create(macos_dir.join("Real"))
            .unwrap()
            .write_all(&[0xfe, 0xed, 0xfa, 0xcf, 0, 0, 0, 0])
            .unwrap();

        let staging_dir = stage_app_for_transfer(&bundle).unwrap();
        let staged_exe = staging_dir.join("Real.app/Contents/MacOS/Real");

        assert!(staged_exe.is_file());
        assert_eq!(
            std::fs::read(&staged_exe).unwrap(),
            [0xfe, 0xed, 0xfa, 0xcf, 0, 0, 0, 0]
        );

        std::fs::remove_dir_all(&source_dir).ok();
        std::fs::remove_dir_all(&staging_dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn staging_preserves_symlinks_instead_of_following_them() {
        let source_dir = temp_dir("symlink-source");
        let bundle = source_dir.join("Linked.app");
        std::fs::create_dir_all(&bundle).unwrap();
        let outside_target = source_dir.join("outside-file");
        std::fs::File::create(&outside_target)
            .unwrap()
            .write_all(b"outside the bundle")
            .unwrap();
        std::os::unix::fs::symlink(&outside_target, bundle.join("link")).unwrap();

        let staging_dir = stage_app_for_transfer(&bundle).unwrap();
        let staged_link = staging_dir.join("Linked.app/link");

        assert!(
            staged_link
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&staged_link).unwrap(), outside_target);

        std::fs::remove_dir_all(&source_dir).ok();
        std::fs::remove_dir_all(&staging_dir).ok();
    }
}
