//! Insula — Milestone 4/5 seam: extracting "just the submitted app's own
//! activity" out of `insula_sensor`'s raw, system-wide `events.jsonl`.
//!
//! Deliberately a downstream, batch pass over the already-captured log
//! rather than live filtering inside the sensor — see `insula_sensor.rs`'s
//! module doc comment for why the sensor logs unfiltered. Doing it here
//! instead means: no new mechanism is needed to tell the sensor which app
//! is being analyzed before it starts, the whole thing is a pure function
//! testable against a fixture log with no VM involved, and the raw log
//! stays available in full in case this pass ever misses something.
//!
//! The algorithm is a single forward pass: find the first `exec` event
//! whose `path` matches the target, seed a "relevant pid" set with it,
//! then walk the rest of the log in order — any event whose actor `pid`
//! is already in the set is kept, and `fork` events additionally add
//! their `child_pid` to the set. This naturally covers descendants at any
//! depth (grandchildren, great-grandchildren, ...), not just direct
//! children, since the set keeps growing as the scan encounters more
//! `fork` events from pids already in it.

use serde_json::Value;
use std::collections::HashSet;

/// Parses `raw_jsonl` (one `serde_json::Value` object per line, as
/// written by `insula_sensor.rs`) and returns only the events attributable
/// to `target_exec_path` and its process descendants, in original order.
///
/// `target_exec_path` should be the exact guest-side path the submitted
/// app will be `execve`'d from — see `vm::expected_guest_exec_path`.
/// Malformed lines are skipped rather than failing the whole pass, since
/// a single corrupt line (e.g. a torn write if the log was read mid-run)
/// shouldn't discard everything captured around it.
///
/// Returns an empty `Vec` if no `exec` event matches `target_exec_path` —
/// this is a real, distinguishable outcome (the app may not have been
/// launched during the observed run yet), not an error.
pub fn filter_for_target(raw_jsonl: &str, target_exec_path: &str) -> Vec<Value> {
    let events: Vec<Value> = raw_jsonl
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    let seed_index = events.iter().position(|event| {
        event["type"] == "exec" && event["path"].as_str() == Some(target_exec_path)
    });
    let Some(seed_index) = seed_index else {
        return Vec::new();
    };

    let mut tracked: HashSet<i64> = HashSet::new();
    tracked.insert(events[seed_index]["pid"].as_i64().unwrap_or(-1));

    let mut matched = Vec::new();
    for event in &events[seed_index..] {
        let pid = event["pid"].as_i64().unwrap_or(-1);
        if !tracked.contains(&pid) {
            continue;
        }
        if event["type"] == "fork"
            && let Some(child_pid) = event["child_pid"].as_i64()
        {
            tracked.insert(child_pid);
        }
        matched.push(event.clone());
    }

    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jsonl(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn no_matching_exec_event_yields_an_empty_result() {
        let log = jsonl(&[
            serde_json::json!({"type": "exec", "pid": 1, "ppid": 0, "path": "/usr/libexec/xpcproxy"}),
        ]);
        assert!(filter_for_target(&log, "/Users/admin/Desktop/Evil.app/Contents/MacOS/Evil").is_empty());
    }

    #[test]
    fn events_before_the_target_launches_are_excluded() {
        let log = jsonl(&[
            serde_json::json!({"type": "exec", "pid": 100, "ppid": 1, "path": "/usr/libexec/xpcproxy"}),
            serde_json::json!({"type": "exit", "pid": 100, "ppid": 1, "status": 0}),
            serde_json::json!({"type": "exec", "pid": 200, "ppid": 501, "path": "/Users/admin/Desktop/Evil.app/Contents/MacOS/Evil"}),
        ]);
        let result = filter_for_target(&log, "/Users/admin/Desktop/Evil.app/Contents/MacOS/Evil");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["pid"], 200);
    }

    #[test]
    fn descendants_at_any_depth_are_included_but_unrelated_pids_are_not() {
        let target = "/Users/admin/Desktop/Evil.app/Contents/MacOS/Evil";
        let log = jsonl(&[
            serde_json::json!({"type": "exec", "pid": 200, "ppid": 501, "path": target}),
            serde_json::json!({"type": "fork", "pid": 200, "ppid": 501, "child_pid": 201}),
            serde_json::json!({"type": "exec", "pid": 201, "ppid": 200, "path": "/bin/sh"}),
            serde_json::json!({"type": "fork", "pid": 201, "ppid": 200, "child_pid": 202}),
            serde_json::json!({"type": "rename", "pid": 202, "ppid": 201, "source_path": "/Users/admin/Documents/report.docx", "destination_path": "/Users/admin/Documents/report.docx.locked"}),
            // Unrelated: a background daemon forking/exec'ing on its own,
            // sharing no ancestry with pid 200 at all.
            serde_json::json!({"type": "fork", "pid": 1, "ppid": 0, "child_pid": 999}),
            serde_json::json!({"type": "exec", "pid": 999, "ppid": 1, "path": "/usr/libexec/xpcproxy"}),
        ]);

        let result = filter_for_target(&log, target);
        let pids: Vec<i64> = result.iter().map(|e| e["pid"].as_i64().unwrap()).collect();

        assert_eq!(pids, vec![200, 200, 201, 201, 202]);
        assert!(!pids.contains(&999));
        assert_eq!(
            result.last().unwrap()["destination_path"],
            "/Users/admin/Documents/report.docx.locked"
        );
    }

    #[test]
    fn malformed_lines_are_skipped_without_failing_the_whole_pass() {
        let target = "/Users/admin/Desktop/Evil.app/Contents/MacOS/Evil";
        let log = format!(
            "not json at all\n{}\n{}",
            serde_json::json!({"type": "exec", "pid": 200, "ppid": 501, "path": target}),
            serde_json::json!({"type": "exit", "pid": 200, "ppid": 501, "status": 0}),
        );

        let result = filter_for_target(&log, target);
        assert_eq!(result.len(), 2);
    }
}
