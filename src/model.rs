//! Core data types shared across collectors and the UI.
//!
//! Every device metric is modeled as [`Opt`] (`Option<T>`) because support is heterogeneous:
//! the same field may be present on one card and "Not supported" / empty on another (e.g. fan
//! and `mem_busy_percent` are absent on the Strix Halo APU we target). Collectors fill what they
//! can and leave the rest `None`; the UI renders `None` as "n/a" and never panics.
//!
//! Memory is stored as raw bytes (`u64`) and only converted to human units at the UI edge.

use serde::Serialize;

/// A metric that may be unsupported on a given device. Renders as "n/a" when `None`.
pub type Opt<T> = Option<T>;

/// Everything we know about one GPU at one instant.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuSnapshot {
    /// DRM card index (the `N` in `cardN`).
    pub index: usize,
    /// Marketing/device name, e.g. "Radeon 8060S Graphics" (from rocm-smi; `None` until enriched).
    pub name: Opt<String>,
    /// PCI device id, e.g. 0x1586.
    pub device_id: Opt<String>,
    /// VBIOS version string.
    pub vbios: Opt<String>,
    /// GPU busy percentage, 0..=100.
    pub busy_pct: Opt<u8>,
    /// Memory pools (VRAM + GTT). On unified-memory APUs VRAM is carved from system RAM.
    pub mem: MemInfo,
    /// Edge temperature in degrees Celsius.
    pub temp_c: Opt<f64>,
    /// Instantaneous socket/package power draw in watts.
    pub power_w: Opt<f64>,
    /// Engine/memory clock frequencies in MHz.
    pub clocks: Clocks,
    /// Rich SMU telemetry decoded from the binary `gpu_metrics` node — APU temp/power split and
    /// per-source throttle accounting. `None` when the node is absent or its revision is unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
}

/// Memory pools. Unified-memory APUs are first-class: VRAM and GTT are tracked separately.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemInfo {
    /// Total VRAM in bytes. On APUs this is carved from system RAM.
    pub vram_total: Opt<u64>,
    /// Used VRAM in bytes.
    pub vram_used: Opt<u64>,
    /// Total GTT (graphics translation table / system-memory aperture) in bytes.
    pub gtt_total: Opt<u64>,
    /// Used GTT in bytes.
    pub gtt_used: Opt<u64>,
}

impl MemInfo {
    /// Fraction of VRAM used in `0.0..=1.0`, or `None` if either side is missing/total is zero.
    pub fn vram_frac(&self) -> Opt<f64> {
        frac(self.vram_used, self.vram_total)
    }

    /// Fraction of GTT used in `0.0..=1.0`, or `None` if either side is missing/total is zero.
    pub fn gtt_frac(&self) -> Opt<f64> {
        frac(self.gtt_used, self.gtt_total)
    }
}

/// Current clock frequencies, in MHz. Names mirror amdgpu's `pp_dpm_*` domains.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Clocks {
    /// Shader/graphics clock.
    pub sclk_mhz: Opt<u32>,
    /// Memory clock.
    pub mclk_mhz: Opt<u32>,
    /// Fabric clock.
    pub fclk_mhz: Opt<u32>,
    /// SoC clock.
    pub socclk_mhz: Opt<u32>,
}

/// SMU telemetry decoded from the binary `gpu_metrics` sysfs node.
///
/// Deliberately scoped to the signals the GPU-centric Gauges/Graphs panels *cannot* show — the
/// rest of the APU sharing one socket: the CPU cores, the NPU (XDNA/IPU), unified-memory
/// bandwidth, the GFX/SoC hotspot temperatures hwmon omits, the sustained-power limit, and
/// per-source throttle accounting. GPU util/clocks/total-power are intentionally left to Gauges.
/// Populated from `gpu_metrics_v3_0` (SMU13 APUs such as Strix Halo); every field is [`Opt`] so an
/// unsupported metric or a different format revision renders as "n/a".
#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    /// GFX (shader) hotspot temperature in degrees Celsius — the throttle-relevant temperature,
    /// distinct from the edge sensor shown in the Gauges panel.
    pub temp_gfx_c: Opt<f64>,
    /// SoC temperature in degrees Celsius.
    pub temp_soc_c: Opt<f64>,
    /// Summed CPU-core power across the socket in watts — the CPU half of the shared APU budget.
    pub cpu_power_w: Opt<f64>,
    /// Highest currently-running CPU-core clock across the socket, in MHz.
    pub cpu_clk_max_mhz: Opt<u16>,
    /// Per-CPU-core C0 (active) residency, 0..=100. Empty when unsupported. The panel summarizes
    /// this as a "busy cores" count.
    pub cpu_core_c0: Vec<u8>,
    /// NPU (XDNA/IPU) activity, 0..=100 — peak across the NPU's columns.
    pub npu_activity_pct: Opt<u16>,
    /// NPU (XDNA/IPU) power in watts.
    pub npu_power_w: Opt<f64>,
    /// Unified-memory read bandwidth in MB/s.
    pub dram_read_mbps: Opt<u16>,
    /// Unified-memory write bandwidth in MB/s.
    pub dram_write_mbps: Opt<u16>,
    /// Sustained (STAPM) power limit in watts; the `0xFFFF` "unset" sentinel renders as n/a.
    pub stapm_limit_w: Opt<f64>,
    /// Cumulative per-source throttle residency counters (monotonic; diffed for "active").
    pub throttle: Throttle,
    /// Throttle sources whose residency advanced over the last interval. Filled by the app from
    /// consecutive samples; empty before a second sample exists, or when nothing throttled.
    pub throttle_active: Vec<String>,
}

/// Cumulative throttle-residency counters from `gpu_metrics`. Each is a free-running accumulator;
/// a source is "currently throttling" when its counter advances between two samples (see
/// [`Throttle::active_since`]). A `None` means the source was absent from the sample.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Throttle {
    /// External PROCHOT assertion.
    pub prochot: Opt<u32>,
    /// Socket power limit (SPL).
    pub spl: Opt<u32>,
    /// Fast PPT limit.
    pub fppt: Opt<u32>,
    /// Slow/sustained PPT limit.
    pub sppt: Opt<u32>,
    /// CPU-core thermal limit.
    pub thm_core: Opt<u32>,
    /// GFX thermal limit.
    pub thm_gfx: Opt<u32>,
    /// SoC thermal limit.
    pub thm_soc: Opt<u32>,
}

impl Throttle {
    /// The throttle sources whose residency increased from `prev` to `self` — i.e. those active
    /// during the interval between the two samples. A source missing from either sample is skipped.
    pub fn active_since(&self, prev: &Throttle) -> Vec<&'static str> {
        let mut active = Vec::new();
        // A source is active when both samples have a value and the residency advanced.
        let mut check = |name, old: Opt<u32>, cur: Opt<u32>| {
            if matches!((old, cur), (Some(p), Some(c)) if c > p) {
                active.push(name);
            }
        };
        check("PROCHOT", prev.prochot, self.prochot);
        check("SPL", prev.spl, self.spl);
        check("FPPT", prev.fppt, self.fppt);
        check("SPPT", prev.sppt, self.sppt);
        check("THM_CORE", prev.thm_core, self.thm_core);
        check("THM_GFX", prev.thm_gfx, self.thm_gfx);
        check("THM_SOC", prev.thm_soc, self.thm_soc);
        active
    }
}

/// Cumulative GPU-engine busy time for a process, in nanoseconds, summed across its DRM clients.
/// These are monotonic counters straight from `fdinfo`; [`crate::app::App`]'s engine sampler diffs
/// two consecutive walks over the wall-clock delta to derive the `*_pct` utilization fields.
/// Internal collector→sampler plumbing — never serialized (see `ProcInfo::engine_ns`).
#[derive(Debug, Clone, Default)]
pub struct EngineNs {
    pub gfx: Opt<u64>,
    pub compute: Opt<u64>,
    pub enc: Opt<u64>,
    pub dec: Opt<u64>,
}

/// Memory attributed to one DRM client (`drm-client-id`) within a process. A process can hold
/// several clients; the detail view lists them so a merged total can be broken down per-context.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProcClient {
    pub client_id: u64,
    pub vram_bytes: Opt<u64>,
    pub gtt_bytes: Opt<u64>,
}

/// One process holding an amdgpu DRM handle (from `/proc/<pid>/fdinfo`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// Full command line from `/proc/<pid>/cmdline` (NUL args joined with spaces). `None` if
    /// unreadable (e.g. kernel thread or vanished pid). Surfaced in the detail view.
    pub cmdline: Opt<String>,
    /// Total GPU memory attributed to this process (VRAM + GTT), in bytes. Derived from the
    /// per-pool figures; retained as the sort key and for `--json` back-compat.
    pub mem_bytes: Opt<u64>,
    /// Device-local VRAM attributed to this process, in bytes.
    pub vram_bytes: Opt<u64>,
    /// GTT (system-RAM GPU pool) attributed to this process, in bytes.
    pub gtt_bytes: Opt<u64>,
    /// Graphics-engine utilization attributed to this process, 0..=100, where derivable.
    pub gfx_pct: Opt<u8>,
    /// Compute-engine utilization, 0..=100, where derivable.
    pub compute_pct: Opt<u8>,
    /// Video-encode-engine utilization, 0..=100, where derivable.
    pub enc_pct: Opt<u8>,
    /// Video-decode-engine utilization, 0..=100, where derivable.
    pub dec_pct: Opt<u8>,
    /// Per-DRM-client memory breakdown (for the detail view); empty if not collected.
    pub clients: Vec<ProcClient>,
    /// Raw cumulative engine counters from the collector, consumed by the sampler to fill the
    /// `*_pct` fields above. Not part of the public `--json` contract.
    #[serde(skip)]
    pub engine_ns: EngineNs,
}

/// Static Vulkan device description (from `vulkaninfo --json`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct VulkanInfo {
    pub device_name: Opt<String>,
    pub driver_name: Opt<String>,
    pub driver_info: Opt<String>,
    pub api_version: Opt<String>,
    /// Device-local memory heaps, sizes in bytes.
    pub heaps_bytes: Vec<u64>,
}

/// Shared used/total → fraction helper. `None` if either is missing or `total == 0`.
fn frac(used: Opt<u64>, total: Opt<u64>) -> Opt<f64> {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => Some(u as f64 / t as f64),
        _ => None,
    }
}

/// Format a byte count as a human-readable binary-unit string (e.g. "23.97 GiB").
///
/// Used only at the UI edge — the model always stores raw bytes. `None` becomes "n/a".
pub fn fmt_bytes(bytes: Opt<u64>) -> String {
    let Some(b) = bytes else {
        return "n/a".to_string();
    };
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = b as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{b} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_frac_basic() {
        let m = MemInfo {
            vram_used: Some(50),
            vram_total: Some(200),
            ..Default::default()
        };
        assert_eq!(m.vram_frac(), Some(0.25));
        assert_eq!(m.gtt_frac(), None); // gtt unset
    }

    #[test]
    fn frac_handles_missing_and_zero() {
        assert_eq!(frac(Some(10), None), None);
        assert_eq!(frac(None, Some(10)), None);
        assert_eq!(frac(Some(10), Some(0)), None); // no divide-by-zero
        assert_eq!(frac(None, None), None);
    }

    #[test]
    fn throttle_active_since_reports_advanced_sources() {
        let prev = Throttle {
            prochot: Some(0),
            spl: Some(100),
            fppt: Some(50),
            sppt: Some(7),
            thm_gfx: Some(9),
            ..Default::default()
        };
        let cur = Throttle {
            prochot: Some(0),  // unchanged → not active
            spl: Some(150),    // advanced → active
            fppt: Some(50),    // unchanged → not active
            sppt: Some(9),     // advanced → active
            thm_gfx: None,     // missing in current sample → skipped
            thm_core: Some(5), // missing in prev → skipped (no baseline)
            ..Default::default()
        };
        assert_eq!(cur.active_since(&prev), vec!["SPL", "SPPT"]);
        // No prior data of any kind → nothing reported.
        assert!(Throttle::default()
            .active_since(&Throttle::default())
            .is_empty());
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(None), "n/a");
        assert_eq!(fmt_bytes(Some(0)), "0 B");
        assert_eq!(fmt_bytes(Some(512)), "512 B");
        assert_eq!(fmt_bytes(Some(1024)), "1.00 KiB");
        assert_eq!(fmt_bytes(Some(1536)), "1.50 KiB");
        // ~24 GiB, matching the probe machine's VRAM usage.
        assert_eq!(fmt_bytes(Some(26_068_107_264)), "24.28 GiB");
        // ~96 GiB total.
        assert_eq!(fmt_bytes(Some(103_079_215_104)), "96.00 GiB");
    }
}
