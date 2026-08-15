//! Insula — Milestone 4: the in-guest observation sensor.
//!
//! Runs *inside* the analysis VM (not on the host, unlike `insula_cli`),
//! using Apple's EndpointSecurity API to observe process/file/network
//! activity — the practical, buildable answer to "watch everything the
//! submitted app causes," see the `project-insula-vm-tooling` memory for
//! why this needs to run in-guest at all (Virtualization.framework
//! exposes no host-side introspection of a running guest) and the exact
//! one-time SIP-disable + Permissive-Security recipe this depends on.
//!
//! **Step 1 of building this**: just prove a client can be created at
//! all — the same check `es-test.c` already proved manually this
//! session, now in Insula's own language. No event subscription yet.

use std::ffi::c_void;

// Hand-written, not bindgen-generated — this first step is small enough
// that transcribing it by hand against Apple's own `EndpointSecurity.h`
// is safe. The block callback's parameters are typed as `c_void` pointers
// rather than a real `es_client_t`/`es_message_t` type: block2 requires
// its closure's argument types to implement `Encode` (Objective-C type
// encoding), which raw pointers to a custom struct don't get for free,
// but `c_void` pointers — the standard opaque-pointer type used
// throughout Objective-C interop — do. A pointer's own layout doesn't
// depend on what it points to, so this is safe even though the handler
// never actually reads through either pointer yet.
#[repr(C)]
struct EsClient {
    _opaque: [u8; 0],
}

// `build.rs` only emits `cargo:rustc-link-search` reliably for this bin
// target (a package's `cargo:rustc-link-lib` seems to only get applied to
// its own `lib` target, not sibling `src/bin/*.rs` binaries — verified via
// `cargo build --verbose`, the `-L` search path reached `insula_sensor`'s
// link step but `-l EndpointSecurity` didn't). Tying the link requirement
// directly to the `extern` block that actually needs it sidesteps that.
#[link(name = "EndpointSecurity")]
unsafe extern "C" {
    /// `es_new_client_result_t es_new_client(es_client_t **client,
    /// es_handler_block_t handler);`
    fn es_new_client(
        client: *mut *mut EsClient,
        handler: &block2::Block<dyn Fn(*mut c_void, *const c_void)>,
    ) -> i32;

    fn es_delete_client(client: *mut EsClient) -> i32;
}

/// `ES_NEW_CLIENT_RESULT_SUCCESS` — the only value this step cares about;
/// the specific failure reason (not entitled, not privileged, ...) isn't
/// distinguished yet.
const ES_NEW_CLIENT_RESULT_SUCCESS: i32 = 0;

fn main() {
    // No-op handler — this step never actually receives an event (no
    // subscription happens), it only checks whether client creation
    // itself is authorized.
    let handler = block2::RcBlock::new(|_client: *mut c_void, _message: *const c_void| {});

    let mut client: *mut EsClient = std::ptr::null_mut();
    let result = unsafe { es_new_client(&mut client, &handler) };

    if result == ES_NEW_CLIENT_RESULT_SUCCESS {
        println!("SUCCESS: ES client created (Rust).");
        unsafe {
            es_delete_client(client);
        }
    } else {
        println!("FAILED: es_new_client returned {result}");
        std::process::exit(1);
    }
}
