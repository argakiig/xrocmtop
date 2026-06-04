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
