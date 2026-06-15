//! Collector for the binary `gpu_metrics` sysfs node.
//!
//! Unlike the text/JSON sources, `gpu_metrics` is a packed C struct the amdgpu driver copies
//! straight from the SMU firmware. It is versioned by a 4-byte [`metrics_table_header`]
//! (`structure_size`, `format_revision`, `content_revision`); the layout that follows is chosen by
//! the revision pair. We currently decode `gpu_metrics_v3_0` (SMU13 APUs such as Strix Halo), which
//! carries the CPU/GPU power split, GFX/SoC hotspot temperatures, and per-source throttle residency
//! that hwmon does not expose on those parts.
//!
//! The struct uses natural C alignment (not `__packed__`): with default alignment
//! `gpu_metrics_v3_0` is exactly 264 bytes, matching the `structure_size` the kernel reports. The
//! parser mirrors that by aligning each read to the field's size. Any out-of-bounds read (a
//! truncated node, or a shorter struct than this revision expects) aborts the parse and yields
//! `None`, which the UI renders as "n/a" — never a panic.
//!
//! Field layout is taken verbatim from the Linux kernel header
//! `drivers/gpu/drm/amd/include/kgd_pp_interface.h` (`struct gpu_metrics_v3_0`). The committed
//! fixture `tests/fixtures/gpu_metrics_v3_0.bin` (a real Strix Halo dump) guards the offsets against
//! ABI drift.

use std::fs;
use std::path::Path;

use crate::model::{Metrics, Opt, Throttle};

/// Read and decode `<device>/gpu_metrics`. `None` if the node is absent/unreadable or its format
/// revision is one we do not decode.
pub fn read(device_dir: &Path) -> Option<Metrics> {
    let bytes = fs::read(device_dir.join("gpu_metrics")).ok()?;
    parse(&bytes)
}

/// Decode a `gpu_metrics` blob, dispatching on the header's revision pair. Pure over `&[u8]` so it
/// is exercised directly against the committed fixture.
pub fn parse(bytes: &[u8]) -> Option<Metrics> {
    // Header: structure_size (u16), format_revision (u8), content_revision (u8).
    let format_revision = *bytes.get(2)?;
    let content_revision = *bytes.get(3)?;
    match (format_revision, content_revision) {
        (3, 0) => parse_v3_0(bytes),
        // Other revisions (v1_x discrete, v2_x older APUs) are not decoded yet → render as n/a.
        _ => None,
    }
}

/// Decode `struct gpu_metrics_v3_0`. Reads sequentially in declaration order, skipping the arrays
/// and fields the UI does not surface while still advancing past them so later offsets stay correct.
fn parse_v3_0(b: &[u8]) -> Option<Metrics> {
    let mut p = 0usize;

    // -- header --
    skip(&mut p, 4); // structure_size, format_revision, content_revision

    // -- Temperature (centi-degrees Celsius) --
    let temp_gfx = rd_u16(b, &mut p)?;
    let temp_soc = rd_u16(b, &mut p)?;
    skip_u16(&mut p, 16); // temperature_core[16] (per-core CPU temps; unpopulated on Strix Halo)
    let _temp_skin = rd_u16(b, &mut p)?;

    // -- Utilization (%) and bandwidth (MB/s) --
    let _gfx_activity = rd_u16(b, &mut p)?; // GPU util is shown by the Gauges panel
    let _vcn_activity = rd_u16(b, &mut p)?;
    let mut npu_activity = 0u16; // peak across average_ipu_activity[8]
    for _ in 0..8 {
        npu_activity = npu_activity.max(rd_u16(b, &mut p)?);
    }
    let mut cpu_core_c0 = Vec::with_capacity(16); // average_core_c0_activity[16]
    for _ in 0..16 {
        cpu_core_c0.push(rd_u16(b, &mut p)?.min(100) as u8);
    }
    let dram_reads = rd_u16(b, &mut p)?;
    let dram_writes = rd_u16(b, &mut p)?;
    let ipu_reads = rd_u16(b, &mut p)?; // average_ipu_reads (MB/s)
    let ipu_writes = rd_u16(b, &mut p)?; // average_ipu_writes (MB/s)

    // -- Driver timestamp --
    let _system_clock_counter = rd_u64(b, &mut p)?;

    // -- Power/Energy (milliwatts) --
    let _socket_power = rd_u32(b, &mut p)?; // ≈ package power shown by the Gauges panel
    let ipu_power = rd_u16(b, &mut p)?;
    let _apu_power = rd_u32(b, &mut p)?;
    let _gfx_power = rd_u32(b, &mut p)?;
    let _dgpu_power = rd_u32(b, &mut p)?;
    let all_core_power = rd_u32(b, &mut p)?;
    skip_u16(&mut p, 16); // average_core_power[16]
    let _sys_power = rd_u16(b, &mut p)?;
    let stapm_power_limit = rd_u16(b, &mut p)?;
    let _current_stapm_power_limit = rd_u16(b, &mut p)?;

    // -- Average clocks (MHz) -- GFX/fabric/memory clocks are shown by the Gauges panel; only the
    // NPU's ipuclk (4th entry) is surfaced here.
    skip_u16(&mut p, 3); // gfxclk, socclk, vpeclk
    let ipuclk = rd_u16(b, &mut p)?;
    skip_u16(&mut p, 4); // fclk, vclk, uclk, mpipu

    // -- Current clocks (MHz) --
    let mut cpu_clk_max = 0u16; // peak across current_coreclk[16]
    for _ in 0..16 {
        cpu_clk_max = cpu_clk_max.max(rd_u16(b, &mut p)?);
    }
    let _current_core_maxfreq = rd_u16(b, &mut p)?;
    let _current_gfx_maxfreq = rd_u16(b, &mut p)?;

    // -- Throttle residency (cumulative counters) --
    let throttle = Throttle {
        prochot: Some(rd_u32(b, &mut p)?),
        spl: Some(rd_u32(b, &mut p)?),
        fppt: Some(rd_u32(b, &mut p)?),
        sppt: Some(rd_u32(b, &mut p)?),
        thm_core: Some(rd_u32(b, &mut p)?),
        thm_gfx: Some(rd_u32(b, &mut p)?),
        thm_soc: Some(rd_u32(b, &mut p)?),
    };
    // time_filter_alphavalue (u32) follows but is not surfaced.

    Some(Metrics {
        temp_gfx_c: centideg(temp_gfx),
        temp_soc_c: centideg(temp_soc),
        cpu_power_w: watts(all_core_power),
        cpu_clk_max_mhz: mhz(cpu_clk_max),
        cpu_core_c0,
        npu_activity_pct: activity(npu_activity),
        npu_power_w: watts_u16(ipu_power),
        npu_clk_mhz: present(ipuclk), // 0 → "0 MHz" (present but idle); only 0xFFFF → n/a
        npu_read_mbps: present(ipu_reads),
        npu_write_mbps: present(ipu_writes),
        dram_read_mbps: present(dram_reads),
        dram_write_mbps: present(dram_writes),
        stapm_limit_w: watts_u16(stapm_power_limit),
        throttle,
        throttle_active: Vec::new(), // derived later by diffing consecutive samples
    })
}

// ---- field interpretation -----------------------------------------------------------------

/// Centi-degree temperature → °C. `0` (unpopulated) and `0xFFFF` (invalid) read as n/a; a running
/// part is never genuinely 0 °C, so treating it as "unsupported" matches the rest of the tool.
fn centideg(v: u16) -> Opt<f64> {
    nz(v).map(|v| f64::from(v) / 100.0)
}

/// Activity percentage. `0` is a valid "idle" reading (kept), but `0xFFFF` means unsupported.
fn activity(v: u16) -> Opt<u16> {
    (v != u16::MAX).then_some(v.min(100))
}

/// A plain u16 reading (e.g. bandwidth in MB/s). `0` is a valid reading (kept); only the `0xFFFF`
/// sentinel reads as n/a.
fn present(v: u16) -> Opt<u16> {
    (v != u16::MAX).then_some(v)
}

/// Milliwatts (u32) → watts; `0` reads as n/a (unpopulated domain).
fn watts(mw: u32) -> Opt<f64> {
    (mw != 0).then(|| f64::from(mw) / 1000.0)
}

/// Milliwatts held in a u16 limit field → watts; `0` and the `0xFFFF` "unset" sentinel read as n/a.
fn watts_u16(mw: u16) -> Opt<f64> {
    nz(mw).map(|v| f64::from(v) / 1000.0)
}

/// MHz frequency; `0` and `0xFFFF` read as n/a.
fn mhz(v: u16) -> Opt<u16> {
    nz(v)
}

/// Map the `0` (unpopulated) and `0xFFFF` (invalid) sentinels to `None`.
fn nz(v: u16) -> Opt<u16> {
    (v != 0 && v != u16::MAX).then_some(v)
}

// ---- little-endian cursor -----------------------------------------------------------------

/// Round `pos` up to the next multiple of `align` (mirrors C struct padding).
fn align(pos: &mut usize, align: usize) {
    let rem = *pos % align;
    if rem != 0 {
        *pos += align - rem;
    }
}

/// Skip `n` raw bytes.
fn skip(pos: &mut usize, n: usize) {
    *pos += n;
}

/// Skip an array of `count` `u16`s (aligning to the element first).
fn skip_u16(pos: &mut usize, count: usize) {
    align(pos, 2);
    *pos += count * 2;
}

fn rd_u16(b: &[u8], pos: &mut usize) -> Option<u16> {
    align(pos, 2);
    let end = *pos + 2;
    let v = u16::from_le_bytes(b.get(*pos..end)?.try_into().ok()?);
    *pos = end;
    Some(v)
}

fn rd_u32(b: &[u8], pos: &mut usize) -> Option<u32> {
    align(pos, 4);
    let end = *pos + 4;
    let v = u32::from_le_bytes(b.get(*pos..end)?.try_into().ok()?);
    *pos = end;
    Some(v)
}

fn rd_u64(b: &[u8], pos: &mut usize) -> Option<u64> {
    align(pos, 8);
    let end = *pos + 8;
    let v = u64::from_le_bytes(b.get(*pos..end)?.try_into().ok()?);
    *pos = end;
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture() -> Vec<u8> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gpu_metrics_v3_0.bin");
        std::fs::read(path).expect("fixture present")
    }

    #[test]
    fn parses_real_v3_0_dump() {
        let m = parse(&fixture()).expect("v3_0 decodes");
        // Temperatures in centi-degrees → °C (hotspot + SoC; not the edge sensor in Gauges).
        assert_eq!(m.temp_gfx_c, Some(70.38));
        assert_eq!(m.temp_soc_c, Some(62.62));
        // CPU half of the shared socket: summed core power (mW → W) and peak core clock.
        assert_eq!(m.cpu_power_w, Some(4.659));
        assert_eq!(m.cpu_clk_max_mhz, Some(5140));
        // Per-core C0 residency, parsed verbatim (16 entries).
        assert_eq!(
            m.cpu_core_c0,
            vec![7, 21, 2, 1, 2, 3, 32, 5, 3, 3, 2, 2, 0, 1, 1, 5]
        );
        // NPU (XDNA/IPU) idle on this dump: 0% activity, 0 W power (→ n/a), but clock and
        // bandwidth are present-and-zero (0 is a valid reading, only 0xFFFF → n/a).
        assert_eq!(m.npu_activity_pct, Some(0));
        assert_eq!(m.npu_power_w, None);
        assert_eq!(m.npu_clk_mhz, Some(0)); // ipuclk=0 → "0 MHz"
        assert_eq!(m.npu_read_mbps, Some(0));
        assert_eq!(m.npu_write_mbps, Some(0));
        // Unified-memory bandwidth in MB/s.
        assert_eq!(m.dram_read_mbps, Some(47791));
        assert_eq!(m.dram_write_mbps, Some(1463));
        // STAPM limit is the 0xFFFF "unset" sentinel on this part → n/a.
        assert_eq!(m.stapm_limit_w, None);
        // Throttle residency counters, parsed verbatim.
        assert_eq!(m.throttle.prochot, Some(0));
        assert_eq!(m.throttle.spl, Some(933_227));
        assert_eq!(m.throttle.fppt, Some(5_965_499));
        assert_eq!(m.throttle.sppt, Some(5_233_158));
        assert_eq!(m.throttle.thm_core, Some(142_287));
        assert_eq!(m.throttle.thm_gfx, Some(68_974));
        assert_eq!(m.throttle.thm_soc, Some(0));
        assert!(m.throttle_active.is_empty()); // not derived at parse time
    }

    #[test]
    fn unsupported_revision_is_none_not_panic() {
        // Same size, but an unknown revision pair (format=1, content=4).
        let mut b = fixture();
        b[2] = 1;
        b[3] = 4;
        assert!(parse(&b).is_none());
    }

    #[test]
    fn truncated_blob_is_none_not_panic() {
        let full = fixture();
        // A header that claims v3_0 but is cut short partway through the struct: every read past
        // the end returns None, so the parse bails cleanly.
        assert!(parse(&full[..32]).is_none());
        // Too short to even hold the header.
        assert!(parse(&full[..2]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn read_missing_node_is_none() {
        let dir = std::env::temp_dir().join("xrocmtop_no_metrics_node");
        let _ = std::fs::create_dir_all(&dir);
        assert!(read(&dir).is_none());
    }
}
