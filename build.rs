//! Links `src/bin/insula_sensor.rs` against Apple's EndpointSecurity C
//! library and generates real Rust bindings for `es_message_t` and the
//! event structs it wraps. See the `project-insula-vm-tooling` memory for
//! why this exists (an in-guest sensor for Milestone 4's observation
//! pipeline) and the exact SIP-disable/entitlement recipe needed to
//! actually load it in the VM.
//!
//! `es_message_t` is a large, version-gated tagged union — hand-
//! transcribing its layout (as step 1 of the sensor did, for the much
//! smaller opaque-pointer-only surface it needed) is exactly the kind of
//! ABI-sensitive struct where a typo causes memory unsafety. `bindgen` is
//! used here to generate the real layout straight from Apple's own
//! header. The one part deliberately *not* run through bindgen is
//! `es_new_client`'s handler, whose type is an Objective-C block
//! (`es_handler_block_t`) — bindgen's block support is unreliable, and
//! `insula_sensor.rs` already has a proven, working hand-written
//! declaration for it via the `block2` crate.

use std::env;
use std::path::PathBuf;
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
    println!("cargo:rustc-link-lib=bsm");
    println!("cargo:rustc-link-search=native={sdk_path}/usr/lib");

    // `cargo:rustc-link-lib` (above) only reliably reaches this project's
    // `lib` target, not the separate `src/bin/*.rs` binary (see the
    // `project-insula-vm-tooling` memory for how this was confirmed via
    // `cargo build --verbose`). `rustc-link-arg-bin` is the general fix —
    // it ties the link flags directly to the `insula_sensor` binary
    // target regardless of which `extern` block in it needs them,
    // covering both the hand-written and bindgen-generated declarations
    // below with one mechanism instead of per-block `#[link(...)]` hacks.
    println!("cargo:rustc-link-arg-bin=insula_sensor=-lEndpointSecurity");
    println!("cargo:rustc-link-arg-bin=insula_sensor=-lbsm");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    let bindings = bindgen::Builder::default()
        .header_contents(
            "insula_es_wrapper.h",
            "#include <EndpointSecurity/EndpointSecurity.h>\n#include <bsm/libbsm.h>\n",
        )
        .clang_arg(format!("-isysroot{sdk_path}"))
        .clang_arg("-fblocks")
        // Pulls in es_message_t and everything it transitively references
        // (es_process_t, es_file_t, es_string_token_t, the full event
        // union, es_event_type_t, ...) — the union covers many event
        // kinds the sensor doesn't subscribe to yet, but there's no cost
        // to having their bindings generated too.
        .allowlist_type("es_message_t")
        .allowlist_type("es_client_t")
        .allowlist_type("es_return_t")
        .allowlist_function("es_subscribe")
        .allowlist_function("es_delete_client")
        .allowlist_function("audit_token_to_pid")
        .generate()
        .expect("bindgen failed to generate EndpointSecurity bindings");

    bindings
        .write_to_file(out_path.join("es_bindings.rs"))
        .expect("failed to write es_bindings.rs");
}
