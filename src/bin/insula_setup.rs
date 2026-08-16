//! Insula — one-time golden-image setup.
//!
//! Turns a freshly-pulled `macos-tahoe-base` into `insula-macos`: a
//! prepared, SIP-disabled clone with the observation sensor
//! (`insula_sensor`) and the app-delivery desktop-sync LaunchDaemon
//! already baked into its disk. This is the *only* place SIP ever gets
//! disabled — every real analysis run clones straight from this already-
//! prepared image (see `src/vm.rs`), so the one truly manual, human-
//! interactive step here (Startup Security Utility, `csrutil disable`)
//! only ever has to happen once per machine, not once per analysis.
//!
//! Run this directly (`cargo run --bin insula_setup`), in a real
//! interactive terminal — it needs to prompt for `sudo` and, the first
//! time, needs you to actually click through Recovery mode yourself.
//! Apple deliberately requires a human there; no amount of scripting gets
//! around it. See the `project-insula-vm-tooling` memory for the exact
//! recipe this automates and the gotchas hit building it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const GOLDEN_VM_NAME: &str = "insula-macos";
const BASE_IMAGE: &str = "ghcr.io/cirruslabs/macos-tahoe-base:latest";
const SETUP_MARKER: &str = "Library/Application Support/Insula/.setup-complete";

const ENTITLEMENTS_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.developer.endpoint-security.client</key>
	<true/>
</dict>
</plist>
"#;

const SENSOR_LAUNCHD_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.insula.sensor</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Library/Application Support/Insula/insula_sensor</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>/tmp/insula-sensor.log</string>
	<key>StandardErrorPath</key>
	<string>/tmp/insula-sensor.log</string>
</dict>
</plist>
"#;

const DESKTOP_SYNC_LAUNCHD_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.insula.desktop-sync</string>
	<key>ProgramArguments</key>
	<array>
		<string>/bin/sh</string>
		<string>/Library/Application Support/Insula/copy-app-to-desktop.sh</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>/tmp/insula-desktop-sync.log</string>
	<key>StandardErrorPath</key>
	<string>/tmp/insula-desktop-sync.log</string>
</dict>
</plist>
"#;

/// `DEST` is built from `insula::vm::GUEST_DESKTOP_DIR` (via `format!` in
/// `desktop_sync_script`, not a literal here) so this stays the single
/// place that string is duplicated into shell — `vm::expected_guest_exec_path`
/// relies on this exact destination to work out what a submitted app's
/// exec path will look like once copied in.
const DESKTOP_SYNC_SCRIPT_TEMPLATE: &str = r#"#!/bin/sh
# Insula — copies the submitted app (shared read-only into this VM via
# Tart's --dir/VirtioFS at boot, see src/vm.rs on the host) onto the
# admin user's Desktop, so it's immediately visible without anyone having
# to go find the shared-volume mount themselves.

SHARE="/Volumes/My Shared Files/insula-app"
DEST="{GUEST_DESKTOP_DIR}"

# The VirtioFS share isn't necessarily mounted the instant this daemon
# starts at boot — poll briefly rather than failing immediately.
i=0
while [ ! -d "$SHARE" ] && [ "$i" -lt 30 ]; do
    sleep 1
    i=$((i + 1))
done

if [ -d "$SHARE" ]; then
    cp -R "$SHARE"/. "$DEST"/
    chown -R admin:staff "$DEST"
    # Anything that arrived via a host-side transfer (as opposed to being
    # compiled in-place on the guest) carries com.apple.quarantine. Left in
    # place, the *first* exec of such a file gets intercepted by Gatekeeper
    # — fine in an interactive session (a prompt or a one-time approval can
    # resolve it), fatal under a headless root LaunchDaemon (no GUI session
    # to resolve it in, so the kernel just SIGKILLs the process outright).
    # Stripped here so both manual and auto-run exploration modes are
    # unaffected.
    xattr -dr com.apple.quarantine "$DEST" 2>/dev/null
fi
"#;

fn desktop_sync_script() -> String {
    DESKTOP_SYNC_SCRIPT_TEMPLATE.replace("{GUEST_DESKTOP_DIR}", insula::vm::GUEST_DESKTOP_DIR)
}

const AUTO_RUN_LAUNCHD_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.insula.autorun</string>
	<key>ProgramArguments</key>
	<array>
		<string>/bin/sh</string>
		<string>/Library/Application Support/Insula/auto-run.sh</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>/tmp/insula-autorun.log</string>
	<key>StandardErrorPath</key>
	<string>/tmp/insula-autorun.log</string>
</dict>
</plist>
"#;

/// The other half of `ExplorationMode::Unattended` (see `vm.rs`'s
/// `RunningVm::launch` for the host side that writes the marker this
/// reads). Deliberately watches the *writable* `insula-logs` share, not
/// the read-only `insula-app` one `copy-app-to-desktop.sh` blanket-copies
/// to Desktop — writing the marker there would leak it onto the Desktop
/// alongside the submitted app. Manual-mode runs never produce this
/// marker, so this daemon just polls once, finds nothing, and exits.
const AUTO_RUN_SCRIPT: &str = r#"#!/bin/sh
MARKER="/Volumes/My Shared Files/insula-logs/.insula-auto-run"

i=0
while [ ! -f "$MARKER" ] && [ "$i" -lt 30 ]; do
    sleep 1
    i=$((i + 1))
done
[ -f "$MARKER" ] || exit 0

TARGET=$(cat "$MARKER")

# The marker can exist before desktop-sync has actually finished copying
# the target file into place — same race, same fix as everywhere else
# here: poll briefly rather than failing immediately. Budget generously
# (90s, not 30s): desktop-sync has its own up-to-30s wait for its share to
# mount, on top of however long the actual copy takes for a large,
# self-contained binary — the two can combine to well past 30s.
i=0
while [ ! -f "$TARGET" ] && [ "$i" -lt 90 ]; do
    sleep 1
    i=$((i + 1))
done

# `-f` only proves cp(1) has *created* the destination — for a large
# self-contained binary (tens of MB) over VirtioFS, the file shows up in
# the directory listing well before all its bytes have landed. Exec'ing it
# then hands the kernel a truncated image whose ad-hoc code-signature
# blob doesn't cover the actual (incomplete) content — caught live as
# "load code signature error 2" in the unified log, followed by an
# instant SIGKILL. So also wait for the size to stop changing across
# consecutive samples before trusting the copy is actually done.
if [ -f "$TARGET" ]; then
    prev_size=-1
    stable=0
    i=0
    while [ "$stable" -lt 3 ] && [ "$i" -lt 120 ]; do
        cur_size=$(wc -c < "$TARGET" 2>/dev/null | tr -d ' ')
        if [ "$cur_size" = "$prev_size" ]; then
            stable=$((stable + 1))
        else
            stable=0
        fi
        prev_size=$cur_size
        sleep 1
        i=$((i + 1))
    done
fi

LOG="/Volumes/My Shared Files/insula-logs/autorun-output.log"

if [ -f "$TARGET" ]; then
    chmod +x "$TARGET" 2>/dev/null
    # Redundant with copy-app-to-desktop.sh's own strip — belt-and-suspenders
    # against the race where this script observes $TARGET existing mid-copy,
    # before that script's later xattr strip has run yet.
    xattr -d com.apple.quarantine "$TARGET" 2>/dev/null
    echo "insula: starting $TARGET" > "$LOG"
    "$TARGET" >> "$LOG" 2>&1
    STATUS=$?
    echo "insula: exited with status $STATUS" >> "$LOG"
    # A shell-reported status >= 128 means the child died from a signal
    # (128 + signal number) rather than exiting on its own — e.g. 137 is
    # SIGKILL. That's the signature of an external enforcement mechanism
    # (Gatekeeper/AMFI/syspolicyd) killing the process outright rather than
    # the app's own logic failing, so pull the kernel's own account of why
    # from the unified log instead of guessing.
    if [ "$STATUS" -ge 128 ] 2>/dev/null; then
        NAME=$(basename "$TARGET")
        echo "insula: signal-terminated, capturing unified log for '$NAME'..." >> "$LOG"
        log show --last 2m --style compact --predicate "eventMessage CONTAINS \"$NAME\" OR process == \"$NAME\"" >> "$LOG" 2>&1
    fi
else
    echo "insula: target never appeared at $TARGET after waiting" > "$LOG"
fi
"#;

fn main() -> anyhow::Result<()> {
    let force = std::env::args().any(|arg| arg == "--force");

    ensure_tart_installed()?;
    ensure_base_image_pulled()?;
    ensure_golden_clone_exists()?;

    let disk_path = vm_disk_path(GOLDEN_VM_NAME);

    if setup_marker_present(&disk_path)? {
        if !force {
            println!(
                "'{GOLDEN_VM_NAME}' is already ready — nothing to do.\n\
                 If the sensor code changed and you want it rewritten onto the golden clone:\n\n  \
                 cargo run --bin insula_setup -- --force\n"
            );
            return Ok(());
        }
        // SIP-disable is a one-time, disk-persisted state — no need to
        // walk through Recovery mode again, only the sensor binary and
        // LaunchDaemons need rewriting. The VM must be stopped for
        // `install_sensor_and_provisioning`'s host-side disk mount to
        // work; `guide_through_sip_disable` normally leaves it stopped,
        // so this path has to do that itself instead.
        println!(
            "--force given: SIP is already disabled so that step is being skipped, \
             just rewriting the sensor and LaunchDaemons..."
        );
        let _ = Command::new("tart").args(["stop", GOLDEN_VM_NAME]).status();
        install_sensor_and_provisioning(&disk_path)?;
        println!("\nUpdate complete.");
        return Ok(());
    }

    guide_through_sip_disable()?;
    install_sensor_and_provisioning(&disk_path)?;

    println!("\nSetup complete. '{GOLDEN_VM_NAME}' is now ready to be cloned for every analysis.");
    Ok(())
}

fn ensure_tart_installed() -> anyhow::Result<()> {
    let found = Command::new("which")
        .arg("tart")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    anyhow::ensure!(
        found,
        "Tart isn't installed. Run this and try again:\n\n  brew install cirruslabs/cli/tart\n"
    );
    Ok(())
}

fn tart_list() -> anyhow::Result<String> {
    let output = Command::new("tart").arg("list").output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn ensure_base_image_pulled() -> anyhow::Result<()> {
    if tart_list()?.contains("macos-tahoe-base") {
        return Ok(());
    }
    println!("Pulling the base macOS image (~27GB compressed, this can take a while)...");
    let status = Command::new("tart").args(["pull", BASE_IMAGE]).status()?;
    anyhow::ensure!(status.success(), "tart pull failed");
    Ok(())
}

fn ensure_golden_clone_exists() -> anyhow::Result<()> {
    let list = tart_list()?;
    let already_exists = list
        .lines()
        .any(|line| line.starts_with("local") && line.contains(GOLDEN_VM_NAME));
    if already_exists {
        return Ok(());
    }
    println!("Creating the '{GOLDEN_VM_NAME}' clone...");
    let status = Command::new("tart")
        .args(["clone", BASE_IMAGE, GOLDEN_VM_NAME])
        .status()?;
    anyhow::ensure!(status.success(), "tart clone failed");
    Ok(())
}

fn vm_disk_path(vm_name: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set");
    PathBuf::from(home)
        .join(".tart/vms")
        .join(vm_name)
        .join("disk.img")
}

fn setup_marker_present(disk_path: &Path) -> anyhow::Result<bool> {
    with_mounted_data_volume(disk_path, |mount_point| {
        Ok(mount_point.join(SETUP_MARKER).exists())
    })
}

/// The one step that can never be scripted — Apple requires an actual
/// human clicking through Recovery mode / Startup Security Utility.
/// Prints exactly what to do and blocks on Enter before continuing.
fn guide_through_sip_disable() -> anyhow::Result<()> {
    println!("\n=== A one-time security setup step is needed ===\n");
    println!("Deep observation (ESF) requires SIP to be disabled inside the VM.");
    println!("This can't be automated — Apple deliberately designed it to require");
    println!("human confirmation. It only ever needs to happen once.\n");
    println!("1. In a separate terminal, run:\n");
    println!("     tart run {GOLDEN_VM_NAME} --recovery --vnc-experimental\n");
    println!("2. After picking a language, from the 'Utilities' menu at the top");
    println!("   open 'Startup Security Utility', select 'Macintosh HD',");
    println!("   authenticate as an admin (admin / admin), leave 'Permissive Security'");
    println!("   selected, check both boxes underneath it, then close.");
    println!("3. Again from the 'Utilities' menu, open 'Terminal', then in order:\n");
    println!("     csrutil disable");
    println!("     csrutil authenticated-root disable");
    println!("     reboot\n");
    println!("4. Once the VM boots normally again, come back here and press Enter.\n");

    print!("Press Enter when ready: ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;

    println!("Continuing — stopping the VM...");
    let _ = Command::new("tart").args(["stop", GOLDEN_VM_NAME]).status();
    Ok(())
}

/// Builds and ad-hoc-signs `insula_sensor`, then writes it plus both
/// LaunchDaemons directly onto the (stopped) golden clone's disk — the
/// same manual recipe this session proved live, now scripted.
fn install_sensor_and_provisioning(disk_path: &Path) -> anyhow::Result<()> {
    println!("Building the sensor...");
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "insula_sensor"])
        .status()?;
    anyhow::ensure!(status.success(), "cargo build failed");
    let sensor_path = PathBuf::from("target/release/insula_sensor");

    let entitlements_path = std::env::temp_dir().join("insula-setup-entitlements.plist");
    std::fs::write(&entitlements_path, ENTITLEMENTS_PLIST)?;
    let status = Command::new("codesign")
        .args(["-s", "-", "-f", "--entitlements"])
        .arg(&entitlements_path)
        .arg(&sensor_path)
        .status()?;
    anyhow::ensure!(status.success(), "codesign failed");

    println!("Files are about to be written, you'll be asked for your password a few times (sudo)...");
    with_mounted_data_volume(disk_path, |mount_point| {
        let insula_dir = mount_point.join("Library/Application Support/Insula");
        let launch_daemons = mount_point.join("Library/LaunchDaemons");

        // `/Library` and `/Library/LaunchDaemons` are `root:wheel`-owned
        // on the guest disk (mounted with real ownership respected, see
        // `with_mounted_data_volume`'s docs) — the current (unprivileged)
        // host user can't write there directly, every write below goes
        // through `sudo`.
        sudo_mkdir_p(&insula_dir)?;
        sudo_mkdir_p(&launch_daemons)?;

        sudo_install_file(&sensor_path, &insula_dir.join("insula_sensor"), "755")?;
        sudo_write_file(
            SENSOR_LAUNCHD_PLIST,
            &launch_daemons.join("com.insula.sensor.plist"),
            "644",
        )?;
        sudo_write_file(
            &desktop_sync_script(),
            &insula_dir.join("copy-app-to-desktop.sh"),
            "755",
        )?;
        sudo_write_file(
            DESKTOP_SYNC_LAUNCHD_PLIST,
            &launch_daemons.join("com.insula.desktop-sync.plist"),
            "644",
        )?;
        sudo_write_file(
            AUTO_RUN_SCRIPT,
            &insula_dir.join("auto-run.sh"),
            "755",
        )?;
        sudo_write_file(
            AUTO_RUN_LAUNCHD_PLIST,
            &launch_daemons.join("com.insula.autorun.plist"),
            "644",
        )?;
        sudo_write_file("", &insula_dir.join(".setup-complete"), "644")?;
        disable_xprotect_and_mrt(mount_point)?;

        Ok(())
    })
}

/// Real filenames confirmed by mounting a golden clone's disk and listing
/// `Library/Apple/System/Library/{LaunchDaemons,LaunchAgents}` directly —
/// not guessed. Note this is `/Library/Apple/System/Library/...`, a
/// separate, Data-volume-resident, independently-updatable tree Apple uses
/// for XProtect/MRT content — distinct from (and not to be confused with)
/// the sealed System volume's `/System/Library/...`.
const XPROTECT_MRT_DAEMONS: &[&str] = &[
    "com.apple.XProtect.daemon.scan.plist",
    "com.apple.XprotectFramework.PluginService.plist",
    "com.apple.MRTd.plist",
    "com.apple.XProtect.daemon.scan.startup.plist",
];
const XPROTECT_MRT_AGENTS: &[&str] = &[
    "com.apple.XProtect.agent.scan.plist",
    "com.apple.XProtect.agent.scan.startup.plist",
    "com.apple.MRTa.plist",
    "com.apple.XprotectFramework.PluginService.plist",
];

/// Insula exists to detonate potentially-malicious submitted binaries
/// inside an isolated, disposable VM and observe what they do — that's
/// the whole point, not an accident. XProtect/MRT (Apple's built-in
/// signature/behavior-based malware scanner) is exactly the kind of thing
/// that gets in the way of that: live-testing showed a bulk file-encrypting
/// test binary getting SIGKILLed within the auto-run window, right as
/// `MRT.app`/`XProtect.app` were observed executing in the sensor's own
/// event log — while the identical binary ran fine under an interactive
/// session (where the very first system malware-scan sweep had already
/// finished by the time a human got around to double-clicking it).
/// Disabled here by renaming each plist with a `.disabled` suffix rather
/// than deleting — launchd only loads files ending in `.plist`, so this is
/// enough to stop them loading at boot, and it's trivially reversible.
/// Runs unconditionally on every setup/`--force` — safe to call again on
/// an already-disabled disk, since `sudo_rename_if_present` is a no-op
/// once the `.plist` is already gone.
fn disable_xprotect_and_mrt(mount_point: &Path) -> anyhow::Result<()> {
    let daemons = mount_point.join("Library/Apple/System/Library/LaunchDaemons");
    let agents = mount_point.join("Library/Apple/System/Library/LaunchAgents");
    for name in XPROTECT_MRT_DAEMONS {
        sudo_rename_if_present(&daemons.join(name))?;
    }
    for name in XPROTECT_MRT_AGENTS {
        sudo_rename_if_present(&agents.join(name))?;
    }
    Ok(())
}

/// `sudo mv <path> <path>.disabled` — only if `<path>` still exists, so
/// re-running setup against an already-disabled disk is a harmless no-op
/// rather than an error.
fn sudo_rename_if_present(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let disabled = path.with_extension("plist.disabled");
    let status = Command::new("sudo").arg("mv").arg(path).arg(&disabled).status()?;
    anyhow::ensure!(status.success(), "failed to disable: {}", path.display());
    Ok(())
}

/// `sudo mkdir -p` + `chown root:wheel` — creating a directory under
/// `/Library` needs root regardless of whether it already exists (an
/// existing directory left over from an earlier, less careful manual
/// attempt might still be host-user-owned, so the `chown` runs unconditionally
/// to fix that too, not just on first creation).
fn sudo_mkdir_p(path: &Path) -> anyhow::Result<()> {
    let status = Command::new("sudo").args(["mkdir", "-p"]).arg(path).status()?;
    anyhow::ensure!(status.success(), "mkdir failed: {}", path.display());
    let status = Command::new("sudo")
        .args(["chown", "root:wheel"])
        .arg(path)
        .status()?;
    anyhow::ensure!(status.success(), "chown failed: {}", path.display());
    Ok(())
}

/// Writes `content` to a temp file as the current (unprivileged) user,
/// then hands it to `sudo_install_file` — the simplest way to get
/// arbitrary content into a root-owned destination without needing a
/// privileged process to hold the content itself.
fn sudo_write_file(content: &str, dest: &Path, mode: &str) -> anyhow::Result<()> {
    let file_name = dest
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("dest has no file name: {}", dest.display()))?;
    let tmp = std::env::temp_dir().join(format!("insula-setup-{}", file_name.to_string_lossy()));
    std::fs::write(&tmp, content)?;
    sudo_install_file(&tmp, dest, mode)
}

/// `sudo cp` + `chown root:wheel` + `chmod` — the standard shape every
/// file this tool places on the guest disk needs, since `/Library` and
/// `/Library/LaunchDaemons` are root-owned there.
fn sudo_install_file(src: &Path, dest: &Path, mode: &str) -> anyhow::Result<()> {
    let status = Command::new("sudo").arg("cp").arg(src).arg(dest).status()?;
    anyhow::ensure!(status.success(), "copy failed: {}", dest.display());
    let status = Command::new("sudo")
        .args(["chown", "root:wheel"])
        .arg(dest)
        .status()?;
    anyhow::ensure!(status.success(), "chown failed: {}", dest.display());
    let status = Command::new("sudo")
        .args(["chmod", mode])
        .arg(dest)
        .status()?;
    anyhow::ensure!(status.success(), "chmod failed: {}", dest.display());
    Ok(())
}

/// Attaches `disk_path` (a *stopped* Tart VM's raw disk image) and mounts
/// its Data volume with real ownership respected (`-mountOptions owners`
/// — a freshly-attached foreign image defaults to `noowners`, under which
/// `chown` silently doesn't persist; this bit the manual version of this
/// recipe earlier this session), runs `f` with the mount point, then
/// always unmounts/detaches afterward even if `f` fails.
fn with_mounted_data_volume<T>(
    disk_path: &Path,
    f: impl FnOnce(&Path) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let attach_output = Command::new("hdiutil")
        .args(["attach", "-nomount"])
        .arg(disk_path)
        .output()?;
    anyhow::ensure!(
        attach_output.status.success(),
        "hdiutil attach failed: {}",
        String::from_utf8_lossy(&attach_output.stderr)
    );
    let attach_text = String::from_utf8_lossy(&attach_output.stdout).to_string();

    // Every top-level (non-partition) identifier hdiutil just created for
    // *this* image — scoping the Data-volume search to only these avoids
    // ever matching the host's own same-named "Data" volume.
    let our_identifiers: Vec<String> = attach_text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|tok| tok.starts_with("/dev/disk"))
        .map(|tok| tok.trim_start_matches("/dev/").to_string())
        .collect();

    let result = (|| -> anyhow::Result<T> {
        let apfs_list = Command::new("diskutil").args(["apfs", "list"]).output()?;
        let apfs_text = String::from_utf8_lossy(&apfs_list.stdout);

        let data_volume = apfs_text
            .lines()
            .filter(|line| line.contains("(Role):") && line.contains("(Data)"))
            .find_map(|line| {
                line.split_whitespace()
                    .find(|tok| our_identifiers.iter().any(|id| id == tok))
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("could not find this image's Data volume"))?;

        let status = Command::new("diskutil")
            .args(["mount", "-mountOptions", "owners"])
            .arg(&data_volume)
            .status()?;
        anyhow::ensure!(status.success(), "diskutil mount failed");

        let mount_point = PathBuf::from("/Volumes/Data");
        let outcome = f(&mount_point);
        let _ = Command::new("diskutil")
            .arg("unmount")
            .arg(&data_volume)
            .status();
        outcome
    })();

    // Detach everything this attach created, regardless of the closure's
    // outcome — a stale attached image left mounted after an error is a
    // real mess to clean up by hand (it also leaves the VM unbootable:
    // Virtualization.framework refuses to start a VM whose disk image is
    // still attached on the host). A top-level identifier here looks like
    // "disk4"; a partition/volume looks like "disk4s1" — checking
    // `!id.contains('s')` to tell them apart is wrong, since the word
    // "disk" itself contains an 's'; this originally left every single
    // identifier filtered out, so nothing ever actually got detached.
    let is_top_level = |id: &&String| {
        id.strip_prefix("disk")
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
    };
    for id in our_identifiers.iter().filter(is_top_level) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-force"])
            .arg(format!("/dev/{id}"))
            .status();
    }

    result
}
