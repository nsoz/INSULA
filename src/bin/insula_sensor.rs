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
//! **Step 3**: subscribes to process lifecycle events
//! (`NOTIFY_EXEC`/`NOTIFY_FORK`/`NOTIFY_EXIT`) *and* the file operations
//! most directly relevant to malicious behavior — mass encryption/deletion
//! in particular — (`NOTIFY_CREATE`/`NOTIFY_RENAME`/`NOTIFY_UNLINK`), and
//! appends one JSON line per event to a writable share the host can read
//! back (`RunningVm::launch` in `src/vm.rs` mounts it in). `WRITE`/`OPEN`
//! stay out of scope deliberately — they fire on effectively every file
//! access system-wide (background indexing, log rotation, ...), which
//! would swamp the far more diagnostic create/rename/unlink signal for
//! comparatively little gain; revisit if create/rename/unlink turns out
//! not to be enough signal on its own.
//!
//! Deliberately logs every event system-wide, not just descendants of the
//! submitted app — the VM is single-purpose and disposable per run, so
//! nothing else of interest is running in it, and matching events back to
//! the target app by process-tree ancestry is a later report-generation
//! concern (see `event_filter.rs`), not something worth tracking as live
//! state in the sensor itself.

use std::ffi::c_void;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

// Real bindings for `es_message_t` and everything it references, generated
// by `build.rs` straight from Apple's own header — see that file's doc
// comment for why (this struct is a large, version-gated tagged union;
// hand-transcribing it is exactly the kind of ABI-sensitive work a typo
// turns into memory unsafety). Wrapped in its own module so the `#[allow]`
// for bindgen's C-style naming doesn't leak into the rest of this file.
mod es_bindings {
    #![allow(
        non_camel_case_types,
        non_upper_case_globals,
        non_snake_case,
        dead_code,
        unsafe_op_in_unsafe_fn
    )]
    include!(concat!(env!("OUT_DIR"), "/es_bindings.rs"));
}
use es_bindings::{
    audit_token_to_pid, es_client_t, es_delete_client,
    es_destination_type_t_ES_DESTINATION_TYPE_EXISTING_FILE, es_event_type_t_ES_EVENT_TYPE_NOTIFY_CREATE,
    es_event_type_t_ES_EVENT_TYPE_NOTIFY_EXEC, es_event_type_t_ES_EVENT_TYPE_NOTIFY_EXIT,
    es_event_type_t_ES_EVENT_TYPE_NOTIFY_FORK, es_event_type_t_ES_EVENT_TYPE_NOTIFY_RENAME,
    es_event_type_t_ES_EVENT_TYPE_NOTIFY_UNLINK, es_file_t, es_message_t, es_process_t,
    es_return_t_ES_RETURN_SUCCESS, es_string_token_t, es_subscribe,
};

// Hand-written, not bindgen-generated — `es_new_client`'s handler type
// (`es_handler_block_t`) is an Objective-C block, which bindgen doesn't
// reliably support. The block callback's parameters are typed as
// `c_void` pointers rather than the real `es_client_t`/`es_message_t`
// pointer types: block2 requires its closure's argument types to
// implement `Encode` (Objective-C type encoding), which raw pointers to a
// custom struct don't get for free, but `c_void` pointers do. A pointer's
// own layout doesn't depend on what it points to, so this is safe even
// though the handler casts back to the real type internally.
//
// `build.rs`'s `cargo:rustc-link-arg-bin=insula_sensor=...` is what
// actually gets this (and the bindgen-generated externs above) linked —
// see its doc comment for why plain `cargo:rustc-link-lib` isn't enough
// for a `src/bin/*.rs` target.
unsafe extern "C" {
    /// `es_new_client_result_t es_new_client(es_client_t **client,
    /// es_handler_block_t handler);`
    fn es_new_client(
        client: *mut *mut es_client_t,
        handler: &block2::Block<dyn Fn(*mut c_void, *const c_void)>,
    ) -> i32;
}

/// `ES_NEW_CLIENT_RESULT_SUCCESS`.
const ES_NEW_CLIENT_RESULT_SUCCESS: i32 = 0;

/// Mirrors the writable share `RunningVm::launch` mounts in on the host
/// side (`src/vm.rs`) — the app-delivery share is read-only and one-way,
/// so a second share is what actually gets events back out of the guest.
const EVENT_LOG_DIR: &str = "/Volumes/My Shared Files/insula-logs";

fn main() {
    // A block's closure must implement `Fn`, not `FnMut` — the ES
    // subsystem is free to invoke it without giving it exclusive access —
    // so mutation of the log file goes through a `Mutex` rather than a
    // plain `let mut`.
    let log = std::sync::Mutex::new(open_event_log());

    let handler = block2::RcBlock::new(move |_client: *mut c_void, message: *const c_void| {
        let message = message as *const es_message_t;
        // SAFETY: `es_new_client`'s contract guarantees `message` is only
        // valid for the duration of this call — every field used below is
        // read and converted to an owned `String`/`serde_json::Value`
        // before the handler returns, nothing borrowed from it escapes.
        let described = unsafe { describe_event(&*message) };
        if let Some(line) = described
            && let Ok(mut log) = log.lock()
        {
            let _ = writeln!(log, "{line}");
            let _ = log.flush();
        }
    });

    let mut client: *mut es_client_t = std::ptr::null_mut();
    let result = unsafe { es_new_client(&mut client, &handler) };
    if result != ES_NEW_CLIENT_RESULT_SUCCESS {
        println!("FAILED: es_new_client returned {result}");
        std::process::exit(1);
    }

    let events = [
        es_event_type_t_ES_EVENT_TYPE_NOTIFY_EXEC,
        es_event_type_t_ES_EVENT_TYPE_NOTIFY_FORK,
        es_event_type_t_ES_EVENT_TYPE_NOTIFY_EXIT,
        es_event_type_t_ES_EVENT_TYPE_NOTIFY_CREATE,
        es_event_type_t_ES_EVENT_TYPE_NOTIFY_RENAME,
        es_event_type_t_ES_EVENT_TYPE_NOTIFY_UNLINK,
    ];
    let subscribe_result =
        unsafe { es_subscribe(client, events.as_ptr(), events.len() as u32) };
    if subscribe_result != es_return_t_ES_RETURN_SUCCESS {
        println!("FAILED: es_subscribe returned {subscribe_result}");
        unsafe {
            es_delete_client(client);
        }
        std::process::exit(1);
    }

    println!("SUCCESS: subscribed to exec/fork/exit/create/rename/unlink (Rust).");

    // The block above does all the real work from here on. This process
    // is started at boot by com.insula.sensor's LaunchDaemon (see
    // insula_setup.rs) and torn down along with the whole disposable VM
    // once the analysis run ends (`RunningVm::drop`, `src/vm.rs`) — there
    // is no separate shutdown signal to wait for, so it just needs to
    // stay alive.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Converts one `es_message_t` into a single JSON line, or `None` for an
/// event type this sensor didn't subscribe to (shouldn't happen given the
/// `es_subscribe` call above, but the union has no safe way to tell "which
/// variant" other than trusting `event_type`, so an unrecognized value is
/// treated as "nothing to log" rather than read as effectively-random
/// bytes).
///
/// # Safety
/// `message` must point to a valid, fully-populated `es_message_t` for the
/// entire call — true for the duration of an ES handler invocation, which
/// is the only place this is called from.
unsafe fn describe_event(message: &es_message_t) -> Option<String> {
    let pid = unsafe { pid_of(message.process) };
    let ppid = unsafe { (*message.process).ppid };
    let time_unix_secs = message.time.tv_sec;

    let value = if message.event_type == es_event_type_t_ES_EVENT_TYPE_NOTIFY_EXEC {
        let target = unsafe { message.event.exec }.target;
        serde_json::json!({
            "type": "exec",
            "pid": pid,
            "ppid": ppid,
            "time_unix_secs": time_unix_secs,
            "path": unsafe { executable_path_of(target) },
        })
    } else if message.event_type == es_event_type_t_ES_EVENT_TYPE_NOTIFY_FORK {
        let child = unsafe { message.event.fork }.child;
        serde_json::json!({
            "type": "fork",
            "pid": pid,
            "ppid": ppid,
            "time_unix_secs": time_unix_secs,
            "child_pid": unsafe { pid_of(child) },
        })
    } else if message.event_type == es_event_type_t_ES_EVENT_TYPE_NOTIFY_EXIT {
        let exit = unsafe { message.event.exit };
        serde_json::json!({
            "type": "exit",
            "pid": pid,
            "ppid": ppid,
            "time_unix_secs": time_unix_secs,
            "status": exit.stat,
        })
    } else if message.event_type == es_event_type_t_ES_EVENT_TYPE_NOTIFY_CREATE {
        let create = unsafe { message.event.create };
        let path = if create.destination_type == es_destination_type_t_ES_DESTINATION_TYPE_EXISTING_FILE
        {
            unsafe { file_path_of(create.destination.existing_file) }
        } else {
            let new_path = unsafe { create.destination.new_path };
            unsafe { joined_path(new_path.dir, &new_path.filename) }
        };
        serde_json::json!({
            "type": "create",
            "pid": pid,
            "ppid": ppid,
            "time_unix_secs": time_unix_secs,
            "path": path,
        })
    } else if message.event_type == es_event_type_t_ES_EVENT_TYPE_NOTIFY_RENAME {
        let rename = unsafe { message.event.rename };
        let destination =
            if rename.destination_type == es_destination_type_t_ES_DESTINATION_TYPE_EXISTING_FILE {
                unsafe { file_path_of(rename.destination.existing_file) }
            } else {
                let new_path = unsafe { rename.destination.new_path };
                unsafe { joined_path(new_path.dir, &new_path.filename) }
            };
        serde_json::json!({
            "type": "rename",
            "pid": pid,
            "ppid": ppid,
            "time_unix_secs": time_unix_secs,
            // The single most ransomware-relevant field in this whole
            // sensor: `source_path` is what the file was called before,
            // `destination_path` is what it got renamed to (e.g. an
            // added `.locked` extension).
            "source_path": unsafe { file_path_of(rename.source) },
            "destination_path": destination,
        })
    } else if message.event_type == es_event_type_t_ES_EVENT_TYPE_NOTIFY_UNLINK {
        let unlink = unsafe { message.event.unlink };
        serde_json::json!({
            "type": "unlink",
            "pid": pid,
            "ppid": ppid,
            "time_unix_secs": time_unix_secs,
            "path": unsafe { file_path_of(unlink.target) },
        })
    } else {
        return None;
    };

    Some(value.to_string())
}

/// `audit_token_to_pid` is the API-sanctioned way to get a pid out of an
/// `es_process_t` (the struct's own `ppid`-style raw field exists mainly
/// for the *parent* pid — see `ESMessageCore.h`'s own doc comment
/// recommending the audit-token accessors over reading fields directly).
///
/// # Safety
/// `process` must be a valid pointer for the duration of the current ES
/// handler call, or null.
unsafe fn pid_of(process: *const es_process_t) -> i32 {
    if process.is_null() {
        return -1;
    }
    unsafe { audit_token_to_pid((*process).audit_token) }
}

/// # Safety
/// `process` must be a valid pointer for the duration of the current ES
/// handler call, or null.
unsafe fn executable_path_of(process: *const es_process_t) -> String {
    if process.is_null() {
        return String::new();
    }
    unsafe { file_path_of((*process).executable) }
}

/// # Safety
/// `file` must be a valid pointer for the duration of the current ES
/// handler call, or null.
unsafe fn file_path_of(file: *const es_file_t) -> String {
    if file.is_null() {
        return String::new();
    }
    unsafe { es_string(&(*file).path) }
}

/// Builds a path out of a parent directory and a filename — needed for
/// the `ES_DESTINATION_TYPE_NEW_PATH` arm of CREATE/RENAME's destination
/// union, where the object being named doesn't have its own `es_file_t`
/// yet (it doesn't exist under that name at the time of the event), so
/// only `dir` + `filename` are available rather than a ready-made path.
///
/// # Safety
/// `dir` must be a valid pointer for the duration of the current ES
/// handler call, or null.
unsafe fn joined_path(dir: *const es_file_t, filename: &es_string_token_t) -> String {
    let dir_path = unsafe { file_path_of(dir) };
    let name = unsafe { es_string(filename) };
    if dir_path.is_empty() {
        name
    } else {
        format!("{dir_path}/{name}")
    }
}

/// `es_string_token_t.length` is documented as "equivalent to strlen()",
/// so `data` need not be null-terminated within that many bytes — read
/// exactly `length` bytes, no more.
///
/// # Safety
/// `token.data` must point to at least `token.length` valid bytes for the
/// duration of the current ES handler call, or be null.
unsafe fn es_string(token: &es_string_token_t) -> String {
    if token.data.is_null() {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(token.data as *const u8, token.length) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// VirtioFS shares aren't necessarily mounted the instant this process
/// starts at boot — same race `insula_setup.rs`'s `DESKTOP_SYNC_SCRIPT`
/// already handles for the app-delivery share, same fix: poll briefly
/// rather than failing immediately.
fn open_event_log() -> std::fs::File {
    let dir = Path::new(EVENT_LOG_DIR);
    for _ in 0..30 {
        if dir.is_dir() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("events.jsonl"))
        .expect("failed to open event log")
}
