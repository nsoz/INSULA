//! Links `src/bin/insula_sensor.rs` against Apple's EndpointSecurity C
//! library. See the `project-insula-vm-tooling` memory for why this
//! exists (an in-guest sensor for Milestone 4's observation pipeline) and
//! the exact SIP-disable/entitlement recipe needed to actually load it in
//! the VM.

use std::process::Command;

fn main() {
    let sdk_path = String::from_utf8(
        Command::new("xcrun")
            .args(["--sdk", "macosx", "--show-sdk-path"])
            .output()
            .expect("xcrun --show-sdk-path failed")
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    println!("cargo:rustc-link-lib=EndpointSecurity");
    println!("cargo:rustc-link-search=native={sdk_path}/usr/lib");
}
