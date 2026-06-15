//! End-to-end contract test for the `--once` scriptable path: run the real binary and assert it
//! exits 0 and emits valid, expected-shaped output. Live values vary by machine, so we assert the
//! contract (exit code, parseable JSON, key presence) rather than exact numbers.

use std::process::Command;

/// Path to the compiled binary, provided by Cargo to integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_xrocmtop");

#[test]
fn once_json_exits_zero_and_is_valid_json() {
    let out = Command::new(BIN)
        .args(["--once", "--json"])
        .output()
        .expect("run binary");
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}",
        out.status
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON on stdout");
    // The "gpus" array is always present (possibly empty on a non-AMD box).
    assert!(json.get("gpus").and_then(|g| g.as_array()).is_some());
}

#[test]
fn once_text_exits_zero() {
    let out = Command::new(BIN)
        .arg("--once")
        .output()
        .expect("run binary");
    assert!(out.status.success());
    // Text mode must not emit a JSON object as its first non-space byte.
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(!stdout.trim_start().starts_with('{'));
}

#[test]
fn once_json_process_entries_carry_expected_fields() {
    // Machine-independent: only assert the schema when this box actually has GPU processes.
    let out = Command::new(BIN)
        .args(["--once", "--json"])
        .output()
        .expect("run binary");
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    let Some(list) = json
        .get("processes")
        .and_then(|p| p.get("list"))
        .and_then(|l| l.as_array())
    else {
        return; // --no-procs not set, but no GPU processes on this box: nothing to assert.
    };
    for entry in list {
        for key in [
            "pid",
            "name",
            "cmdline",
            "mem_bytes",
            "vram_bytes",
            "gtt_bytes",
            "gfx_pct",
            "compute_pct",
            "enc_pct",
            "dec_pct",
            "clients",
        ] {
            assert!(
                entry.get(key).is_some(),
                "process entry missing contract key `{key}`: {entry}"
            );
        }
        // The raw engine-ns plumbing must NOT leak into the public JSON.
        assert!(
            entry.get("engine_ns").is_none(),
            "engine_ns is internal and must not be serialized"
        );
    }
}

#[test]
fn once_json_omits_session_only_thermal_events() {
    // The thermal-events log is in-memory and session-scoped; it must never enter the scriptable
    // snapshot contract (it isn't even Serialize). Guard against an accidental future leak.
    let out = Command::new(BIN)
        .args(["--once", "--json"])
        .output()
        .expect("run binary");
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(json.get("thermal_events").is_none());
    assert!(json.get("events").is_none());
    if let Some(gpus) = json.get("gpus").and_then(|g| g.as_array()) {
        for gpu in gpus {
            assert!(
                gpu.get("thermal_events").is_none() && gpu.get("events").is_none(),
                "per-GPU snapshot must not carry the session thermal-events log: {gpu}"
            );
        }
    }
}

#[test]
fn once_json_metrics_carry_npu_fields() {
    // When a GPU exposes the decoded `gpu_metrics` block, it must carry the NPU telemetry keys
    // (activity/power plus the added clock and read/write bandwidth). Machine-independent: only
    // asserted on boxes that actually decode a metrics block.
    let out = Command::new(BIN)
        .args(["--once", "--json"])
        .output()
        .expect("run binary");
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    let Some(gpus) = json.get("gpus").and_then(|g| g.as_array()) else {
        return;
    };
    for gpu in gpus {
        let Some(metrics) = gpu.get("metrics").filter(|m| !m.is_null()) else {
            continue; // no gpu_metrics node / unsupported revision on this box.
        };
        for key in [
            "npu_activity_pct",
            "npu_power_w",
            "npu_clk_mhz",
            "npu_read_mbps",
            "npu_write_mbps",
        ] {
            assert!(
                metrics.get(key).is_some(),
                "metrics block missing NPU contract key `{key}`: {metrics}"
            );
        }
    }
}

#[test]
fn once_json_respects_no_procs_and_no_vulkan() {
    let out = Command::new(BIN)
        .args(["--once", "--json", "--no-procs", "--no-vulkan"])
        .output()
        .expect("run binary");
    assert!(out.status.success());
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(
        json.get("processes").is_none(),
        "--no-procs omits processes"
    );
    assert!(json.get("vulkan").is_none(), "--no-vulkan omits vulkan");
}
