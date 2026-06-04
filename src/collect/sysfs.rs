//! Primary collector: amdgpu sysfs + its hwmon node.
//!
//! This is the hot-path source — plain file reads, no subprocess. Every field is optional: a
//! missing, empty, or unparsable file yields `None` rather than an error, because amdgpu exposes
//! different subsets per card and kernel. Parser functions are pure (`&str`/`&Path`) so they can
//! be exercised against captured fixtures, including deliberately degraded ones.
//!
//! Field reference (probe machine, Strix Halo):
//! - `gpu_busy_percent`           → integer 0..=100
//! - `mem_info_{vram,gtt}_{total,used}` → bytes
//! - `pp_dpm_{sclk,mclk,fclk,socclk}`   → lines "`N: <MHz>Mhz`", active level marked `*`
//! - `hwmon/hwmonN/temp1_input`   → milli-degrees Celsius
//! - `hwmon/hwmonN/power1_average`→ micro-watts (fallback `power1_input`)

use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{Clocks, GpuSnapshot, MemInfo, Opt};

/// A discovered amdgpu DRM card, identified by its sysfs `device` directory.
#[derive(Debug, Clone)]
pub struct SysfsGpu {
    pub index: usize,
    pub device_dir: PathBuf,
}

impl SysfsGpu {
    /// Read a full snapshot from sysfs. Identity fields (name/vbios/ids) stay `None` here — they
    /// are filled by the rocm-smi collector (T7).
    pub fn read(&self) -> GpuSnapshot {
        let dev = &self.device_dir;
        let hwmon = find_hwmon(dev);
        GpuSnapshot {
            index: self.index,
            busy_pct: read_u64(&dev.join("gpu_busy_percent")).and_then(|v| u8::try_from(v).ok()),
            mem: read_mem(dev),
            temp_c: hwmon.as_deref().and_then(read_temp_c),
            power_w: hwmon.as_deref().and_then(read_power_w),
            clocks: read_clocks(dev),
            ..Default::default()
        }
    }
}

/// Enumerate amdgpu cards under the real DRM root (`/sys/class/drm`).
pub fn enumerate() -> Vec<SysfsGpu> {
    enumerate_in(Path::new("/sys/class/drm"))
}

/// Enumerate amdgpu cards under an arbitrary DRM root. Factored out so tests can point at a
/// fixture tree. Cards are returned sorted by index for stable ordering.
pub fn enumerate_in(drm_root: &Path) -> Vec<SysfsGpu> {
    let mut gpus = Vec::new();
    let Ok(entries) = fs::read_dir(drm_root) else {
        return gpus;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Match "cardN" exactly (skip "card0-eDP-1" connector dirs and "renderD*").
        let Some(idx) = name
            .strip_prefix("card")
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        let device_dir = entry.path().join("device");
        if is_amdgpu(&device_dir) {
            gpus.push(SysfsGpu {
                index: idx,
                device_dir,
            });
        }
    }
    gpus.sort_by_key(|g| g.index);
    gpus
}

/// Heuristic: a `device` dir is an amdgpu GPU if its hwmon node is named "amdgpu", or (fallback)
/// it exposes the amdgpu-specific `mem_info_vram_total` file.
fn is_amdgpu(device_dir: &Path) -> bool {
    if let Some(hwmon) = find_hwmon(device_dir) {
        if read_string(&hwmon.join("name")).as_deref() == Some("amdgpu") {
            return true;
        }
    }
    device_dir.join("mem_info_vram_total").exists()
}

fn read_mem(dev: &Path) -> MemInfo {
    MemInfo {
        vram_total: read_u64(&dev.join("mem_info_vram_total")),
        vram_used: read_u64(&dev.join("mem_info_vram_used")),
        gtt_total: read_u64(&dev.join("mem_info_gtt_total")),
        gtt_used: read_u64(&dev.join("mem_info_gtt_used")),
    }
}

fn read_clocks(dev: &Path) -> Clocks {
    let active = |domain: &str| {
        read_string(&dev.join(format!("pp_dpm_{domain}")))
            .as_deref()
            .and_then(parse_active_clock)
    };
    Clocks {
        sclk_mhz: active("sclk"),
        mclk_mhz: active("mclk"),
        fclk_mhz: active("fclk"),
        socclk_mhz: active("socclk"),
    }
}

/// Edge temperature in Celsius from `temp1_input` (milli-degrees).
fn read_temp_c(hwmon: &Path) -> Opt<f64> {
    read_u64(&hwmon.join("temp1_input")).map(|milli| milli as f64 / 1000.0)
}

/// Power draw in watts, preferring `power1_average`, falling back to `power1_input` (micro-watts).
fn read_power_w(hwmon: &Path) -> Opt<f64> {
    read_u64(&hwmon.join("power1_average"))
        .or_else(|| read_u64(&hwmon.join("power1_input")))
        .map(|micro| micro as f64 / 1_000_000.0)
}

/// First `hwmon/hwmon*` subdirectory of a device dir, if any.
fn find_hwmon(device_dir: &Path) -> Opt<PathBuf> {
    let entries = fs::read_dir(device_dir.join("hwmon")).ok()?;
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("hwmon"))
        })
        .min() // deterministic pick when several exist
}

// ---- pure parsers -------------------------------------------------------------------------

/// Parse the active clock (MHz) from a `pp_dpm_*` listing: the line ending with `*`.
///
/// ```text
/// 0: 400Mhz
/// 1: 1000Mhz
/// 7: 2000Mhz *   <- active
/// ```
fn parse_active_clock(content: &str) -> Opt<u32> {
    let line = content.lines().find(|l| l.trim_end().ends_with('*'))?;
    parse_mhz(line)
}

/// Extract the integer MHz value from a clock line like "`7: 2000Mhz *`".
fn parse_mhz(line: &str) -> Opt<u32> {
    let lower = line.to_ascii_lowercase();
    let idx = lower.find("mhz")?;
    lower[..idx]
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
}

// ---- file helpers -------------------------------------------------------------------------

/// Read and trim a sysfs file to a `String`; `None` if absent/unreadable/empty.
fn read_string(path: &Path) -> Opt<String> {
    let s = fs::read_to_string(path).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Read a sysfs file as `u64`; `None` if absent/unreadable/empty/unparsable.
fn read_u64(path: &Path) -> Opt<u64> {
    read_string(path)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sysfs")
    }

    #[test]
    fn parse_active_clock_picks_starred_line() {
        let s = "0: 400Mhz \n1: 1000Mhz \n7: 2000Mhz *\n";
        assert_eq!(parse_active_clock(s), Some(2000));
    }

    #[test]
    fn parse_active_clock_none_when_unmarked() {
        assert_eq!(parse_active_clock("0: 400Mhz \n1: 1000Mhz \n"), None);
        assert_eq!(parse_active_clock(""), None);
    }

    #[test]
    fn parse_mhz_extracts_value() {
        assert_eq!(parse_mhz("2: 2900Mhz *"), Some(2900));
        assert_eq!(parse_mhz("6: 1472Mhz *"), Some(1472));
        assert_eq!(parse_mhz("garbage"), None);
    }

    #[test]
    fn reads_full_snapshot_from_real_fixture() {
        let gpus = enumerate_in(&fixtures().join("drm"));
        assert_eq!(gpus.len(), 1, "expected one amdgpu card in fixture");
        let snap = gpus[0].read();
        assert_eq!(snap.index, 0);
        assert_eq!(snap.busy_pct, Some(0));
        assert_eq!(snap.mem.vram_total, Some(103_079_215_104));
        assert_eq!(snap.mem.vram_used, Some(20_902_731_776));
        assert_eq!(snap.mem.gtt_total, Some(16_368_283_648));
        assert_eq!(snap.temp_c, Some(35.0)); // temp1_input fixture = 35000 m°C
        assert_eq!(snap.power_w, Some(16.02)); // power1_average fixture = 16020000 µW
        assert_eq!(snap.clocks.sclk_mhz, Some(2900));
        assert_eq!(snap.clocks.mclk_mhz, Some(937));
        assert_eq!(snap.clocks.fclk_mhz, Some(2000));
        assert_eq!(snap.clocks.socclk_mhz, Some(1472));
    }

    #[test]
    fn degraded_card_yields_none_not_panic() {
        let gpus = enumerate_in(&fixtures().join("degraded"));
        assert_eq!(gpus.len(), 1, "degraded card still detected via mem_info");
        let snap = gpus[0].read();
        // Only vram_total exists; everything else is absent or empty.
        assert_eq!(snap.mem.vram_total, Some(103_079_215_104));
        assert_eq!(snap.mem.vram_used, None);
        assert_eq!(snap.busy_pct, None);
        assert_eq!(snap.temp_c, None);
        assert_eq!(snap.power_w, None);
        assert_eq!(snap.clocks.sclk_mhz, None);
    }

    #[test]
    fn empty_drm_root_is_empty_not_error() {
        let tmp = std::env::temp_dir().join("xrocmtop_empty_drm_test");
        let _ = fs::create_dir_all(&tmp);
        assert!(enumerate_in(&tmp).is_empty());
    }
}
