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
fi
"#;

fn desktop_sync_script() -> String {
    DESKTOP_SYNC_SCRIPT_TEMPLATE.replace("{GUEST_DESKTOP_DIR}", insula::vm::GUEST_DESKTOP_DIR)
}

fn main() -> anyhow::Result<()> {
    let force = std::env::args().any(|arg| arg == "--force");

    ensure_tart_installed()?;
    ensure_base_image_pulled()?;
    ensure_golden_clone_exists()?;

    let disk_path = vm_disk_path(GOLDEN_VM_NAME);

    if setup_marker_present(&disk_path)? {
        if !force {
            println!(
                "'{GOLDEN_VM_NAME}' zaten hazır — yapılacak bir şey yok.\n\
                 Sensör kodu değişti ve golden clone'a yeniden yazılsın istiyorsan:\n\n  \
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
            "--force verildi: SIP zaten kapalı olduğu için o adım atlanıyor, \
             sadece sensör ve LaunchDaemon'lar yeniden yazılıyor..."
        );
        let _ = Command::new("tart").args(["stop", GOLDEN_VM_NAME]).status();
        install_sensor_and_provisioning(&disk_path)?;
        println!("\nGüncelleme tamamlandı.");
        return Ok(());
    }

    guide_through_sip_disable()?;
    install_sensor_and_provisioning(&disk_path)?;

    println!("\nKurulum tamamlandı. '{GOLDEN_VM_NAME}' artık her analiz için klonlanmaya hazır.");
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
        "Tart kurulu değil. Şunu çalıştırıp tekrar dene:\n\n  brew install cirruslabs/cli/tart\n"
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
    println!("Temel macOS imajı indiriliyor (~27GB sıkıştırılmış, biraz sürebilir)...");
    let status = Command::new("tart").args(["pull", BASE_IMAGE]).status()?;
    anyhow::ensure!(status.success(), "tart pull başarısız oldu");
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
    println!("'{GOLDEN_VM_NAME}' klonu oluşturuluyor...");
    let status = Command::new("tart")
        .args(["clone", BASE_IMAGE, GOLDEN_VM_NAME])
        .status()?;
    anyhow::ensure!(status.success(), "tart clone başarısız oldu");
    Ok(())
}

fn vm_disk_path(vm_name: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME set olmalı");
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
    println!("\n=== Bir kerelik güvenlik kurulumu gerekiyor ===\n");
    println!("Derin gözlem (ESF) için VM içinde SIP'in kapatılması gerekiyor.");
    println!("Bu, otomatikleştirilemez — Apple bunu bilinçli olarak insan");
    println!("onayı gerektirecek şekilde tasarlamış. Sadece bir kere yapılır.\n");
    println!("1. Ayrı bir terminalde şunu çalıştır:\n");
    println!("     tart run {GOLDEN_VM_NAME} --recovery --vnc-experimental\n");
    println!("2. Dil seçtikten sonra üstteki 'Utilities' menüsünden");
    println!("   'Startup Security Utility'yi aç, 'Macintosh HD'yi seç,");
    println!("   yönetici olarak doğrula (admin / admin), 'Permissive Security'");
    println!("   seçili kalsın, altındaki iki kutucuğu da işaretle, kapat.");
    println!("3. Yine 'Utilities' menüsünden 'Terminal'i aç, sırayla:\n");
    println!("     csrutil disable");
    println!("     csrutil authenticated-root disable");
    println!("     reboot\n");
    println!("4. VM normal şekilde yeniden açılınca buraya dön ve Enter'a bas.\n");

    print!("Hazır olduğunda Enter'a bas: ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;

    println!("Devam ediliyor — VM durduruluyor...");
    let _ = Command::new("tart").args(["stop", GOLDEN_VM_NAME]).status();
    Ok(())
}

/// Builds and ad-hoc-signs `insula_sensor`, then writes it plus both
/// LaunchDaemons directly onto the (stopped) golden clone's disk — the
/// same manual recipe this session proved live, now scripted.
fn install_sensor_and_provisioning(disk_path: &Path) -> anyhow::Result<()> {
    println!("Sensör derleniyor...");
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "insula_sensor"])
        .status()?;
    anyhow::ensure!(status.success(), "cargo build başarısız oldu");
    let sensor_path = PathBuf::from("target/release/insula_sensor");

    let entitlements_path = std::env::temp_dir().join("insula-setup-entitlements.plist");
    std::fs::write(&entitlements_path, ENTITLEMENTS_PLIST)?;
    let status = Command::new("codesign")
        .args(["-s", "-", "-f", "--entitlements"])
        .arg(&entitlements_path)
        .arg(&sensor_path)
        .status()?;
    anyhow::ensure!(status.success(), "codesign başarısız oldu");

    println!("Dosyalar yazılacak, parolan birkaç kere istenecek (sudo)...");
    with_mounted_data_volume(disk_path, |mount_point| {
        let insula_dir = mount_point.join("Library/Application Support/Insula");
        let launch_daemons = mount_point.join("Library/LaunchDaemons");

        // `/Library` and `/Library/LaunchDaemons` are `root:wheel`-owned
        // on the guest disk (mounted with real ownership respected, see
        // `with_mounted_data_volume`'s docs) — nsoz can't write there
        // directly, every write below goes through `sudo`.
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
        sudo_write_file("", &insula_dir.join(".setup-complete"), "644")?;

        Ok(())
    })
}

/// `sudo mkdir -p` + `chown root:wheel` — creating a directory under
/// `/Library` needs root regardless of whether it already exists (an
/// existing directory left over from an earlier, less careful manual
/// attempt might still be nsoz-owned, so the `chown` runs unconditionally
/// to fix that too, not just on first creation).
fn sudo_mkdir_p(path: &Path) -> anyhow::Result<()> {
    let status = Command::new("sudo").args(["mkdir", "-p"]).arg(path).status()?;
    anyhow::ensure!(status.success(), "mkdir başarısız: {}", path.display());
    let status = Command::new("sudo")
        .args(["chown", "root:wheel"])
        .arg(path)
        .status()?;
    anyhow::ensure!(status.success(), "chown başarısız: {}", path.display());
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
    anyhow::ensure!(status.success(), "kopyalama başarısız: {}", dest.display());
    let status = Command::new("sudo")
        .args(["chown", "root:wheel"])
        .arg(dest)
        .status()?;
    anyhow::ensure!(status.success(), "chown başarısız: {}", dest.display());
    let status = Command::new("sudo")
        .args(["chmod", mode])
        .arg(dest)
        .status()?;
    anyhow::ensure!(status.success(), "chmod başarısız: {}", dest.display());
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
        "hdiutil attach başarısız: {}",
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
            .ok_or_else(|| anyhow::anyhow!("bu imajın Data volume'ı bulunamadı"))?;

        let status = Command::new("diskutil")
            .args(["mount", "-mountOptions", "owners"])
            .arg(&data_volume)
            .status()?;
        anyhow::ensure!(status.success(), "diskutil mount başarısız oldu");

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
