//! Supplemental collector: `rocm-smi --json`.
//!
//! sysfs ([`super::sysfs`]) is the authoritative, hot-path source. This collector forks the
//! `rocm-smi` binary at a *low* cadence to fill the STATIC identity fields sysfs lacks
//! (`name`, `device_id`, `vbios`) and, only where sysfs left a value `None`, to supplement live
//! metrics. sysfs always wins: [`merge`] never overwrites an existing `Some`.
//!
//! Design: a pure parser ([`parse_smi_json`]) over a captured `&str` is unit-tested against a
//! committed fixture and needs no binary present. The runner ([`collect`]) feature-detects the
//! binary and degrades to an empty map on any failure (absent, non-zero exit, garbage output) so
//! the app never errors.
//!
//! ## Output shape
//!
//! `rocm-smi` keys each card as `"cardN"` and maps human-readable field labels to *string* values
//! (numbers are quoted). Unsupported metrics appear literally as `"N/A"` or `"Not supported"` and
//! must be treated as absent. Note the plain `rocm-smi --json` (concise table) is rejected by the
//! tool with `NOT_SUPPORTED`; JSON only works with explicit `--show*` sub-flags, so the runner
//! passes the static + supplemental flag set.
//!
//! ```text
//! {"card0": {"Device Name": "Radeon 8060S Graphics", "Device ID": "0x1586",
//!            "VBIOS version": "113-STRXLGEN-001", "GPU use (%)": "0",
//!            "sclk clock speed:": "(2900Mhz)", "VRAM Total Memory (B)": "103079215104", ...}}
//! ```

use std::collections::BTreeMap;
use std::process::Command;

use serde_json::Value;

use crate::model::{Clocks, GpuSnapshot, MemInfo, Opt};

/// Flags passed to `rocm-smi` to emit the static-identity + supplemental JSON the parser consumes.
/// Plain `--json` is rejected for concise output, so each datum is requested explicitly.
const SMI_ARGS: &[&str] = &[
    "--showproductname",
    "--showvbios",
    "--showid",
    "--showtemp",
    "--showpower",
    "--showuse",
    "--showmeminfo",
    "vram",
    "gtt",
    "--showclocks",
    "--json",
];

/// Parsed `rocm-smi` data for a single card. Every field is [`Opt`]; `"N/A"` / `"Not supported"` /
/// missing keys all become `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SmiData {
    // --- static identity (sysfs lacks these) ---
    pub name: Opt<String>,
    pub device_id: Opt<String>,
    pub vbios: Opt<String>,
    // --- supplemental live metrics (sysfs is authoritative; only used to fill `None`) ---
    pub busy_pct: Opt<u8>,
    pub temp_c: Opt<f64>,
    pub power_w: Opt<f64>,
    pub vram_total: Opt<u64>,
    pub vram_used: Opt<u64>,
    pub gtt_total: Opt<u64>,
    pub gtt_used: Opt<u64>,
    pub sclk_mhz: Opt<u32>,
    pub mclk_mhz: Opt<u32>,
    pub fclk_mhz: Opt<u32>,
    pub socclk_mhz: Opt<u32>,
}

/// Run `rocm-smi` and parse its JSON into per-card data keyed by card index.
///
/// Total and never-erroring: a missing binary, non-zero exit, non-UTF-8 or garbage output all
/// yield an empty map. Not unit-tested (forks a process); the parsing it delegates to is.
pub fn collect() -> BTreeMap<usize, SmiData> {
    let Ok(output) = Command::new("rocm-smi").args(SMI_ARGS).output() else {
        return BTreeMap::new(); // binary absent or could not spawn
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return BTreeMap::new();
    };
    parse_smi_json(&text)
}

/// Pure parser: captured `rocm-smi --json` text → per-card [`SmiData`] keyed by `cardN` index.
///
/// Tolerant by construction: non-JSON input, a non-object root, non-`cardN` keys, and unsupported
/// (`"N/A"` / `"Not supported"`) or unparsable values are all skipped, never panicking. An empty
/// or unparseable document yields an empty map.
pub fn parse_smi_json(json: &str) -> BTreeMap<usize, SmiData> {
    let mut out = BTreeMap::new();
    let Ok(root) = serde_json::from_str::<Value>(json) else {
        return out;
    };
    let Some(cards) = root.as_object() else {
        return out;
    };
    for (key, card) in cards {
        let Some(index) = key
            .strip_prefix("card")
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue; // ignore non-"cardN" keys (e.g. "system")
        };
        let Some(fields) = card.as_object() else {
            continue;
        };
        out.insert(index, parse_card(fields));
    }
    out
}

/// Parse one card's flat label→value object into [`SmiData`]. rocm-smi exposes a couple of
/// alternate labels across versions, so identity lookups try each in turn.
fn parse_card(fields: &serde_json::Map<String, Value>) -> SmiData {
    let s = |key: &str| field_str(fields, key);
    SmiData {
        name: s("Device Name").or_else(|| s("Card Series")),
        device_id: s("Device ID").or_else(|| s("Card Model")),
        vbios: s("VBIOS version"),
        busy_pct: field_num::<f64>(fields, "GPU use (%)").and_then(|v| u8::try_from(v as u64).ok()),
        temp_c: field_num::<f64>(fields, "Temperature (Sensor edge) (C)"),
        power_w: field_num::<f64>(fields, "Current Socket Graphics Package Power (W)")
            .or_else(|| field_num::<f64>(fields, "Average Graphics Package Power (W)")),
        vram_total: field_num::<u64>(fields, "VRAM Total Memory (B)"),
        vram_used: field_num::<u64>(fields, "VRAM Total Used Memory (B)"),
        gtt_total: field_num::<u64>(fields, "GTT Total Memory (B)"),
        gtt_used: field_num::<u64>(fields, "GTT Total Used Memory (B)"),
        sclk_mhz: clock_mhz(fields, "sclk"),
        mclk_mhz: clock_mhz(fields, "mclk"),
        fclk_mhz: clock_mhz(fields, "fclk"),
        socclk_mhz: clock_mhz(fields, "socclk"),
    }
}

/// Merge parsed `rocm-smi` data into a sysfs-derived snapshot.
///
/// Static identity (`name`/`device_id`/`vbios`) is owned by rocm-smi and filled if present. Live
/// metrics are only written where the snapshot left `None` — **sysfs is authoritative and is never
/// clobbered**. Idempotent: re-merging the same data changes nothing.
pub fn merge(snap: &mut GpuSnapshot, data: &SmiData) {
    fill(&mut snap.name, &data.name);
    fill(&mut snap.device_id, &data.device_id);
    fill(&mut snap.vbios, &data.vbios);

    fill_copy(&mut snap.busy_pct, data.busy_pct);
    fill_copy(&mut snap.temp_c, data.temp_c);
    fill_copy(&mut snap.power_w, data.power_w);

    let MemInfo {
        vram_total,
        vram_used,
        gtt_total,
        gtt_used,
    } = &mut snap.mem;
    fill_copy(vram_total, data.vram_total);
    fill_copy(vram_used, data.vram_used);
    fill_copy(gtt_total, data.gtt_total);
    fill_copy(gtt_used, data.gtt_used);

    let Clocks {
        sclk_mhz,
        mclk_mhz,
        fclk_mhz,
        socclk_mhz,
    } = &mut snap.clocks;
    fill_copy(sclk_mhz, data.sclk_mhz);
    fill_copy(mclk_mhz, data.mclk_mhz);
    fill_copy(fclk_mhz, data.fclk_mhz);
    fill_copy(socclk_mhz, data.socclk_mhz);
}

// ---- merge helpers ------------------------------------------------------------------------

/// Set `dst` to a clone of `src` only if `dst` is currently `None` and `src` is `Some`.
fn fill(dst: &mut Opt<String>, src: &Opt<String>) {
    if dst.is_none() {
        if let Some(v) = src {
            *dst = Some(v.clone());
        }
    }
}

/// `Copy` variant of [`fill`] for scalar metrics.
fn fill_copy<T: Copy>(dst: &mut Opt<T>, src: Opt<T>) {
    if dst.is_none() {
        if let Some(v) = src {
            *dst = Some(v);
        }
    }
}

// ---- pure value extraction ---------------------------------------------------------------

/// True for rocm-smi's "unsupported" sentinels, which must be treated as absent.
fn is_unsupported(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t.eq_ignore_ascii_case("N/A") || t.eq_ignore_ascii_case("Not supported")
}

/// Fetch a field as a trimmed `String`, mapping unsupported sentinels to `None`. rocm-smi quotes
/// every value, but tolerate a raw JSON number too.
fn field_str(fields: &serde_json::Map<String, Value>, key: &str) -> Opt<String> {
    let v = fields.get(key)?;
    let s = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if is_unsupported(&s) {
        None
    } else {
        Some(s)
    }
}

/// Fetch a numeric field, parsing rocm-smi's quoted strings (and bare JSON numbers). Unsupported
/// sentinels and unparsable text yield `None`.
fn field_num<T: std::str::FromStr>(fields: &serde_json::Map<String, Value>, key: &str) -> Opt<T> {
    field_str(fields, key)?.parse::<T>().ok()
}

/// Parse a clock speed for `domain` from rocm-smi's `"<domain> clock speed:"` key, whose value
/// looks like `"(2900Mhz)"`. Extracts the leading integer MHz; `None` if absent/unsupported.
fn clock_mhz(fields: &serde_json::Map<String, Value>, domain: &str) -> Opt<u32> {
    let raw = field_str(fields, &format!("{domain} clock speed:"))?;
    let lower = raw.to_ascii_lowercase();
    let idx = lower.find("mhz")?;
    lower[..idx]
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture() -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rocm-smi.json");
        std::fs::read_to_string(p).expect("read rocm-smi fixture")
    }

    #[test]
    fn parses_static_identity_from_fixture() {
        let map = parse_smi_json(&fixture());
        let c0 = map.get(&0).expect("card0 present");
        assert_eq!(c0.name.as_deref(), Some("Radeon 8060S Graphics"));
        assert_eq!(c0.device_id.as_deref(), Some("0x1586"));
        assert_eq!(c0.vbios.as_deref(), Some("113-STRXLGEN-001"));
    }

    #[test]
    fn parses_supplemental_live_fields_from_fixture() {
        let map = parse_smi_json(&fixture());
        let c0 = &map[&0];
        assert_eq!(c0.busy_pct, Some(0));
        assert_eq!(c0.temp_c, Some(36.0));
        assert_eq!(c0.power_w, Some(18.007));
        assert_eq!(c0.vram_total, Some(103_079_215_104));
        assert_eq!(c0.vram_used, Some(50_349_752_320));
        assert_eq!(c0.gtt_total, Some(16_368_283_648));
        assert_eq!(c0.gtt_used, Some(2_118_324_224));
        assert_eq!(c0.sclk_mhz, Some(2900));
        assert_eq!(c0.mclk_mhz, Some(937));
        assert_eq!(c0.fclk_mhz, Some(2000));
        assert_eq!(c0.socclk_mhz, Some(1472));
    }

    #[test]
    fn card_index_parsed_from_key() {
        let map = parse_smi_json(&fixture());
        assert!(map.contains_key(&0));
        assert!(map.contains_key(&1));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn unsupported_sentinels_become_none() {
        // card1 in the fixture carries "N/A" / "Not supported" for its live fields.
        let map = parse_smi_json(&fixture());
        let c1 = &map[&1];
        // Static identity still present.
        assert_eq!(c1.name.as_deref(), Some("Radeon RX 7900 XTX"));
        assert_eq!(c1.device_id.as_deref(), Some("0x744c"));
        assert_eq!(c1.vbios.as_deref(), Some("113-D7060100-105"));
        // Unsupported live metrics → None, no panic.
        assert_eq!(c1.busy_pct, None); // "GPU use (%)": "N/A"
        assert_eq!(c1.temp_c, None); // "...": "N/A"
        assert_eq!(c1.power_w, None); // "...": "Not supported"
        assert_eq!(c1.vram_used, None); // "...": "N/A"
                                        // A real value alongside the sentinels still parses.
        assert_eq!(c1.vram_total, Some(25_753_026_560));
    }

    #[test]
    fn garbage_input_returns_empty_no_panic() {
        assert!(parse_smi_json("").is_empty());
        assert!(parse_smi_json("not json at all").is_empty());
        assert!(parse_smi_json("[1, 2, 3]").is_empty()); // root not an object
        assert!(parse_smi_json("{\"system\": {\"x\": \"y\"}}").is_empty()); // no cardN keys
        assert!(parse_smi_json("{\"card0\": 42}").is_empty()); // card not an object
        assert!(parse_smi_json("{\"cardX\": {}}").is_empty()); // unparsable index
    }

    #[test]
    fn missing_keys_yield_none_not_panic() {
        let map = parse_smi_json("{\"card0\": {}}");
        let c0 = &map[&0];
        assert_eq!(c0, &SmiData::default());
    }

    #[test]
    fn merge_fills_identity_and_empty_live_fields() {
        let mut snap = GpuSnapshot {
            index: 0,
            ..Default::default()
        };
        let data = SmiData {
            name: Some("Radeon 8060S Graphics".into()),
            device_id: Some("0x1586".into()),
            vbios: Some("113-STRXLGEN-001".into()),
            busy_pct: Some(7),
            temp_c: Some(36.0),
            power_w: Some(18.0),
            vram_total: Some(100),
            vram_used: Some(50),
            sclk_mhz: Some(2900),
            ..Default::default()
        };
        merge(&mut snap, &data);
        assert_eq!(snap.name.as_deref(), Some("Radeon 8060S Graphics"));
        assert_eq!(snap.device_id.as_deref(), Some("0x1586"));
        assert_eq!(snap.vbios.as_deref(), Some("113-STRXLGEN-001"));
        assert_eq!(snap.busy_pct, Some(7));
        assert_eq!(snap.temp_c, Some(36.0));
        assert_eq!(snap.power_w, Some(18.0));
        assert_eq!(snap.mem.vram_total, Some(100));
        assert_eq!(snap.mem.vram_used, Some(50));
        assert_eq!(snap.clocks.sclk_mhz, Some(2900));
    }

    #[test]
    fn merge_does_not_clobber_existing_sysfs_values() {
        // sysfs already populated the live fields; rocm-smi must not overwrite them.
        let mut snap = GpuSnapshot {
            index: 0,
            busy_pct: Some(3),
            temp_c: Some(35.0),
            power_w: Some(16.02),
            mem: MemInfo {
                vram_total: Some(103_079_215_104),
                vram_used: Some(20_902_731_776),
                gtt_total: Some(16_368_283_648),
                gtt_used: Some(1),
            },
            clocks: Clocks {
                sclk_mhz: Some(2900),
                mclk_mhz: Some(937),
                fclk_mhz: Some(2000),
                socclk_mhz: Some(1472),
            },
            ..Default::default()
        };
        let data = SmiData {
            // Identity is still filled (sysfs leaves it None).
            name: Some("Radeon 8060S Graphics".into()),
            device_id: Some("0x1586".into()),
            vbios: Some("113-STRXLGEN-001".into()),
            // Different live values that must be ignored.
            busy_pct: Some(99),
            temp_c: Some(99.0),
            power_w: Some(999.0),
            vram_total: Some(1),
            vram_used: Some(2),
            gtt_total: Some(3),
            gtt_used: Some(4),
            sclk_mhz: Some(1),
            mclk_mhz: Some(2),
            fclk_mhz: Some(3),
            socclk_mhz: Some(4),
        };
        merge(&mut snap, &data);
        // Identity filled.
        assert_eq!(snap.name.as_deref(), Some("Radeon 8060S Graphics"));
        assert_eq!(snap.device_id.as_deref(), Some("0x1586"));
        assert_eq!(snap.vbios.as_deref(), Some("113-STRXLGEN-001"));
        // Live values untouched (sysfs authoritative).
        assert_eq!(snap.busy_pct, Some(3));
        assert_eq!(snap.temp_c, Some(35.0));
        assert_eq!(snap.power_w, Some(16.02));
        assert_eq!(snap.mem.vram_total, Some(103_079_215_104));
        assert_eq!(snap.mem.vram_used, Some(20_902_731_776));
        assert_eq!(snap.mem.gtt_used, Some(1));
        assert_eq!(snap.clocks.sclk_mhz, Some(2900));
        assert_eq!(snap.clocks.socclk_mhz, Some(1472));
    }

    #[test]
    fn merge_with_default_data_is_noop() {
        let mut snap = GpuSnapshot {
            index: 0,
            busy_pct: Some(5),
            ..Default::default()
        };
        merge(&mut snap, &SmiData::default());
        assert_eq!(snap.name, None);
        assert_eq!(snap.busy_pct, Some(5));
    }
}
