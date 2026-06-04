//! The `--once` snapshot path: collect one sample and emit it as text or JSON, then exit. This is
//! the scriptable contract — no TUI, stable JSON shape — so other tools can consume GPU state.

use serde::Serialize;

use crate::app::App;
use crate::model::{fmt_bytes, GpuSnapshot, Opt, ProcInfo, VulkanInfo};

/// A single point-in-time snapshot of everything the tool observes. Serialized verbatim for
/// `--once --json`; the field names are the public contract.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub gpus: Vec<GpuSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<ProcessReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vulkan: Option<VulkanInfo>,
}

/// Per-process accounting plus the count hidden behind permissions.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessReport {
    pub hidden: usize,
    pub list: Vec<ProcInfo>,
}

impl Report {
    /// Build a report from an already-ticked [`App`], honoring `--no-procs` / `--no-vulkan`.
    pub fn from_app(app: &App) -> Self {
        Self {
            gpus: app.snapshots().to_vec(),
            processes: app.show_procs().then(|| ProcessReport {
                hidden: app.procs_hidden(),
                list: app.procs().to_vec(),
            }),
            vulkan: app.vulkan().cloned(),
        }
    }

    /// Pretty JSON. Falls back to `{}` only if serialization somehow fails (it won't for these
    /// plain data types).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Human-readable multi-line summary.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if self.gpus.is_empty() {
            out.push_str("No AMD GPUs detected.\n");
        }
        for g in &self.gpus {
            out.push_str(&gpu_text(g));
        }
        if let Some(p) = &self.processes {
            out.push('\n');
            out.push_str(&process_text(p));
        }
        if let Some(v) = &self.vulkan {
            out.push('\n');
            out.push_str(&vulkan_text(v));
        }
        out
    }
}

fn gpu_text(g: &GpuSnapshot) -> String {
    let name = g.name.as_deref().unwrap_or("AMD GPU");
    let vram_pct = pct(g.mem.vram_used, g.mem.vram_total);
    let gtt_pct = pct(g.mem.gtt_used, g.mem.gtt_total);
    format!(
        "GPU {idx}: {name}\n  \
         Util: {util}   Temp: {temp}   Power: {power}\n  \
         VRAM: {vu} / {vt}{vp}\n  \
         GTT:  {gu} / {gt}{gp}\n  \
         Clocks: sclk {s}  mclk {m}  fclk {f}  socclk {soc}\n",
        idx = g.index,
        util = opt_pct(g.busy_pct),
        temp = g.temp_c.map_or("n/a".into(), |t| format!("{t:.0}°C")),
        power = g.power_w.map_or("n/a".into(), |w| format!("{w:.1} W")),
        vu = fmt_bytes(g.mem.vram_used),
        vt = fmt_bytes(g.mem.vram_total),
        vp = vram_pct,
        gu = fmt_bytes(g.mem.gtt_used),
        gt = fmt_bytes(g.mem.gtt_total),
        gp = gtt_pct,
        s = mhz(g.clocks.sclk_mhz),
        m = mhz(g.clocks.mclk_mhz),
        f = mhz(g.clocks.fclk_mhz),
        soc = mhz(g.clocks.socclk_mhz),
    )
}

fn process_text(p: &ProcessReport) -> String {
    let mut out = if p.hidden > 0 {
        format!("Processes ({}, +{} hidden):\n", p.list.len(), p.hidden)
    } else {
        format!("Processes ({}):\n", p.list.len())
    };
    for proc in &p.list {
        out.push_str(&format!(
            "  {:<8} {:<24} {}\n",
            proc.pid,
            proc.name,
            fmt_bytes(proc.mem_bytes)
        ));
    }
    out
}

fn vulkan_text(v: &VulkanInfo) -> String {
    let driver = match (&v.driver_name, &v.driver_info) {
        (Some(n), Some(i)) => format!("{n} ({i})"),
        (Some(n), None) => n.clone(),
        (None, Some(i)) => i.clone(),
        (None, None) => "n/a".to_string(),
    };
    format!(
        "Vulkan: {}\n  Driver: {}   API: {}\n",
        v.device_name.as_deref().unwrap_or("n/a"),
        driver,
        v.api_version.as_deref().unwrap_or("n/a"),
    )
}

fn opt_pct(p: Opt<u8>) -> String {
    p.map_or("n/a".to_string(), |p| format!("{p}%"))
}

fn mhz(v: Opt<u32>) -> String {
    v.map_or("n/a".to_string(), |m| format!("{m}MHz"))
}

/// " (NN%)" suffix, or empty when the fraction can't be computed.
fn pct(used: Opt<u64>, total: Opt<u64>) -> String {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => format!(" ({}%)", u as u128 * 100 / t as u128),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clocks, MemInfo};
    use std::path::Path;

    fn sample_report() -> Report {
        Report {
            gpus: vec![GpuSnapshot {
                index: 0,
                name: Some("Radeon 8060S Graphics".into()),
                device_id: Some("0x1586".into()),
                vbios: Some("113-STRXLGEN-001".into()),
                busy_pct: Some(42),
                mem: MemInfo {
                    vram_total: Some(103_079_215_104),
                    vram_used: Some(51_539_607_552),
                    gtt_total: Some(16_368_283_648),
                    gtt_used: Some(2_118_324_224),
                },
                temp_c: Some(42.0),
                power_w: Some(27.1),
                clocks: Clocks {
                    sclk_mhz: Some(2900),
                    mclk_mhz: Some(937),
                    fclk_mhz: Some(2000),
                    socclk_mhz: Some(1472),
                },
            }],
            processes: Some(ProcessReport {
                hidden: 3,
                list: vec![ProcInfo {
                    pid: 693842,
                    name: "llama-server".into(),
                    mem_bytes: Some(31_086_206_976),
                    vram_bytes: Some(30_000_000_000),
                    gtt_bytes: Some(1_086_206_976),
                    ..Default::default()
                }],
            }),
            vulkan: Some(VulkanInfo {
                device_name: Some("Radeon 8060S Graphics (RADV STRIX_HALO)".into()),
                driver_name: Some("radv".into()),
                driver_info: Some("Mesa 26.0.3-1ubuntu1".into()),
                api_version: Some("1.4.335".into()),
                heaps_bytes: vec![],
            }),
        }
    }

    #[test]
    fn json_matches_committed_contract_fixture() {
        let expected = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/once.json"),
        )
        .expect("fixture present");
        // Compare structurally so trailing-newline/format noise doesn't make the test brittle.
        let got: serde_json::Value = serde_json::from_str(&sample_report().to_json()).unwrap();
        let want: serde_json::Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn json_is_valid_and_has_expected_keys() {
        let v: serde_json::Value = serde_json::from_str(&sample_report().to_json()).unwrap();
        assert!(v.get("gpus").and_then(|g| g.as_array()).is_some());
        assert_eq!(v["gpus"][0]["name"], "Radeon 8060S Graphics");
        assert_eq!(v["processes"]["hidden"], 3);
        assert_eq!(v["vulkan"]["api_version"], "1.4.335");
        // Split memory and engine breakdown are part of the public contract; the internal
        // engine_ns counter is not.
        let proc = &v["processes"]["list"][0];
        assert_eq!(proc["vram_bytes"], 30_000_000_000u64);
        assert_eq!(proc["gtt_bytes"], 1_086_206_976u64);
        assert!(proc.get("compute_pct").is_some());
        assert!(proc.get("clients").and_then(|c| c.as_array()).is_some());
        assert!(proc.get("engine_ns").is_none());
    }

    #[test]
    fn no_procs_and_no_vulkan_are_omitted() {
        let r = Report {
            gpus: vec![],
            processes: None,
            vulkan: None,
        };
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert!(v.get("processes").is_none());
        assert!(v.get("vulkan").is_none());
    }

    #[test]
    fn text_summary_contains_key_fields() {
        let t = sample_report().to_text();
        assert!(t.contains("GPU 0: Radeon 8060S Graphics"));
        assert!(t.contains("Util: 42%"));
        assert!(t.contains("VRAM: 48.00 GiB / 96.00 GiB (50%)"));
        assert!(t.contains("Processes (1, +3 hidden)"));
        assert!(t.contains("llama-server"));
        assert!(t.contains("Vulkan: Radeon 8060S Graphics (RADV STRIX_HALO)"));
        assert!(t.contains("API: 1.4.335"));
    }

    #[test]
    fn pct_large_values_do_not_overflow() {
        // used * 100 overflows u64 (2e17 * 100 = 2e19 > u64::MAX ~1.84e19);
        // the u128 math keeps the percentage correct.
        assert_eq!(
            pct(Some(200_000_000_000_000_000), Some(400_000_000_000_000_000)),
            " (50%)"
        );
    }
}
