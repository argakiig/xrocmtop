//! One-shot Vulkan device collector: parses `vulkaninfo --json`.
//!
//! Vulkan device info is essentially static, so this runs **once** at startup (the runner) and the
//! result is cached. The binary may be absent and its JSON schema varies a lot between versions
//! (the classic flat `VkPhysicalDeviceProperties` top-level vs. the newer Vulkan Profiles
//! `capabilities.device.properties` wrapper), so everything here is defensive: a missing binary,
//! a non-zero exit, or garbage JSON yields `None`/empty — never a panic.
//!
//! The parser is a pure `fn parse_vulkaninfo(&str) -> Option<VulkanInfo>` driven by the committed
//! fixture `tests/fixtures/vulkaninfo.json`; the runner does I/O and is not unit-tested.
//!
//! JSON paths consumed (version-sensitive, looked up tolerantly anywhere in the tree):
//! - `VkPhysicalDeviceProperties.deviceName`           → device name
//! - `VkPhysicalDeviceProperties.apiVersion` (integer) → decoded `major.minor.patch`
//! - `driverName` / `driverInfo` (from `VkPhysicalDeviceVulkan12Properties` or
//!   `VkPhysicalDeviceDriverProperties`)               → driver identity
//! - `VkPhysicalDeviceMemoryProperties.memoryHeaps[].{size,flags}` → DEVICE_LOCAL heap sizes

use std::process::Command;

use serde_json::Value;

use crate::model::VulkanInfo;

/// Run `vulkaninfo --json` once and parse it. Returns `None` if the binary is missing, exits
/// non-zero, or emits unparsable output.
///
/// `vulkaninfo --json` does NOT print to stdout: it writes a `VP_VULKANINFO_<device>_<ver>.json`
/// file into the current working directory (`--json=<N>` selects a GPU number, not a path). So we
/// run it inside a throwaway temp directory, read back the single file it produced, parse it, and
/// clean up. Any failure (binary absent, non-zero exit, no file, unparsable) yields `None`. The
/// runner is intentionally not unit-tested — the parse logic it delegates to is.
pub fn collect() -> Option<VulkanInfo> {
    let dir = TempDir::new("xrocmtop-vulkaninfo")?;
    let status = Command::new("vulkaninfo")
        .arg("--json")
        .current_dir(dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let json = read_vp_json(dir.path())?;
    parse_vulkaninfo(&json)
}

/// Read the first `VP_VULKANINFO_*.json` file vulkaninfo dropped in `dir`.
fn read_vp_json(dir: &std::path::Path) -> Option<String> {
    let entry = std::fs::read_dir(dir).ok()?.flatten().find(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("VP_VULKANINFO") && n.ends_with(".json"))
    })?;
    std::fs::read_to_string(entry.path()).ok()
}

/// A uniquely-named temp directory removed on drop, so a failed/cleaned vulkaninfo run never
/// leaves `VP_VULKANINFO_*.json` litter behind.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Option<Self> {
        // process id + a monotonic counter give a unique path without needing rand/time, which
        // are unavailable in this codebase's constraints anyway.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        // Use `create_dir` (NOT `create_dir_all`): it fails with `AlreadyExists` if the final
        // component is already present, so the directory is created *exclusively* by us. In a
        // world-writable /tmp this defeats the predictable-path race — a local actor that
        // pre-creates the path (as a dir or symlink) only forces a retry instead of letting us
        // adopt their directory. The parent (`temp_dir()`) is assumed to exist; we create only the
        // final component. On collision, bump the counter and retry up to a bounded number of times.
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{prefix}-{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Some(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Pure parser: turn a `vulkaninfo --json` document into a [`VulkanInfo`].
///
/// Tolerant by design — the document is walked with [`serde_json::Value`] and the fields we care
/// about are found by key regardless of how the surrounding schema nests them. Returns `None` only
/// when the input is not valid JSON; an otherwise-empty document yields a default `VulkanInfo`
/// (all `None`/empty) so the UI can still render its "unavailable"-ish panel.
pub fn parse_vulkaninfo(json: &str) -> Option<VulkanInfo> {
    let root: Value = serde_json::from_str(json).ok()?;

    // The physical-device properties block, wherever it sits in the schema.
    let dev_props = find_object_with_key(&root, "deviceName");

    let device_name = dev_props
        .and_then(|o| o.get("deviceName"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let api_version = dev_props
        .and_then(|o| o.get("apiVersion"))
        .and_then(Value::as_u64)
        .map(decode_api_version);

    // Driver name/info may live in VkPhysicalDeviceVulkan12Properties or
    // VkPhysicalDeviceDriverProperties — search by the leaf key directly.
    let driver_name = find_string_by_key(&root, "driverName");
    let driver_info = find_string_by_key(&root, "driverInfo");

    let heaps_bytes = find_value_by_key(&root, "memoryHeaps")
        .map(device_local_heap_sizes)
        .unwrap_or_default();

    Some(VulkanInfo {
        device_name,
        driver_name,
        driver_info,
        api_version,
        heaps_bytes,
    })
}

/// Decode a packed Vulkan `apiVersion`/`VK_MAKE_VERSION` integer into `"major.minor.patch"`.
///
/// `major = ver >> 22`, `minor = (ver >> 12) & 0x3ff`, `patch = ver & 0xfff`.
fn decode_api_version(ver: u64) -> String {
    let major = ver >> 22;
    let minor = (ver >> 12) & 0x3ff;
    let patch = ver & 0xfff;
    format!("{major}.{minor}.{patch}")
}

/// Collect the `size` of every DEVICE_LOCAL memory heap from a `memoryHeaps` value.
///
/// `memoryHeaps` is an array of `{ "size": <bytes>, "flags": [...] }`. A heap counts as
/// device-local when any flag string contains `DEVICE_LOCAL` (covers both
/// `"MEMORY_HEAP_DEVICE_LOCAL_BIT"` and the short `"DEVICE_LOCAL"` forms emitted across versions).
/// If `flags` is absent we cannot prove it is device-local, so it is skipped.
fn device_local_heap_sizes(heaps: &Value) -> Vec<u64> {
    let Some(arr) = heaps.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|heap| heap_is_device_local(heap))
        .filter_map(|heap| heap.get("size").and_then(Value::as_u64))
        .collect()
}

/// True when a heap object's `flags` mention DEVICE_LOCAL. Flags may be an array of strings or a
/// single string depending on the emitter; both are handled.
fn heap_is_device_local(heap: &Value) -> bool {
    let Some(flags) = heap.get("flags") else {
        return false;
    };
    fn mentions(s: &str) -> bool {
        s.contains("DEVICE_LOCAL")
    }
    match flags {
        Value::Array(items) => items.iter().filter_map(Value::as_str).any(mentions),
        Value::String(s) => mentions(s),
        _ => false,
    }
}

// ---- tolerant lookups -----------------------------------------------------------------------

/// First object anywhere in the tree that directly contains `key`. Depth-first.
fn find_object_with_key<'a>(
    value: &'a Value,
    key: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => {
            if map.contains_key(key) {
                return Some(map);
            }
            map.values().find_map(|v| find_object_with_key(v, key))
        }
        Value::Array(items) => items.iter().find_map(|v| find_object_with_key(v, key)),
        _ => None,
    }
}

/// First value anywhere in the tree stored directly under `key`. Depth-first.
fn find_value_by_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if let Some(v) = map.get(key) {
                return Some(v);
            }
            map.values().find_map(|v| find_value_by_key(v, key))
        }
        Value::Array(items) => items.iter().find_map(|v| find_value_by_key(v, key)),
        _ => None,
    }
}

/// First string value anywhere in the tree stored directly under `key`.
fn find_string_by_key(value: &Value, key: &str) -> Option<String> {
    find_value_by_key(value, key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vulkaninfo.json");
        std::fs::read_to_string(path).expect("fixture present")
    }

    #[test]
    fn parses_real_trimmed_fixture() {
        let info = parse_vulkaninfo(&fixture()).expect("valid json");
        assert_eq!(
            info.device_name.as_deref(),
            Some("Radeon 8060S Graphics (RADV STRIX_HALO)")
        );
        assert_eq!(info.driver_name.as_deref(), Some("radv"));
        assert_eq!(info.driver_info.as_deref(), Some("Mesa 26.0.3-1ubuntu1"));
        // apiVersion 4211023 = 0x00404_14F → major 1, minor 4, patch 335.
        assert_eq!(info.api_version.as_deref(), Some("1.4.335"));
        // Only heap[1] is DEVICE_LOCAL; heap[0] (no flag) is excluded.
        assert_eq!(info.heaps_bytes, vec![79_631_667_200]);
    }

    #[test]
    fn decode_api_version_uses_vk_version_macros() {
        // Real Strix Halo apiVersion (4211023) → 1.4.335; verify the bit math directly.
        assert_eq!(decode_api_version(4211023), "1.4.335");
        assert_eq!(decode_api_version((1 << 22) | (2 << 12) | 131), "1.2.131");
        assert_eq!(decode_api_version(0), "0.0.0");
    }

    #[test]
    fn parses_classic_flat_schema() {
        // Older vulkaninfo emitted a flat top-level layout without the capabilities wrapper.
        let json = r#"{
            "VkPhysicalDeviceProperties": { "deviceName": "Some GPU", "apiVersion": 4206592 },
            "VkPhysicalDeviceDriverProperties": { "driverName": "amdvlk", "driverInfo": "1.2.3" },
            "VkPhysicalDeviceMemoryProperties": {
                "memoryHeaps": [
                    { "size": 8589934592, "flags": ["MEMORY_HEAP_DEVICE_LOCAL_BIT"] },
                    { "size": 268435456, "flags": [] }
                ]
            }
        }"#;
        let info = parse_vulkaninfo(json).unwrap();
        assert_eq!(info.device_name.as_deref(), Some("Some GPU"));
        assert_eq!(info.driver_name.as_deref(), Some("amdvlk"));
        assert_eq!(info.api_version.as_deref(), Some("1.3.0"));
        assert_eq!(info.heaps_bytes, vec![8_589_934_592]);
    }

    #[test]
    fn flags_as_single_string_is_handled() {
        let json = r#"{
            "VkPhysicalDeviceMemoryProperties": {
                "memoryHeaps": [ { "size": 100, "flags": "DEVICE_LOCAL" } ]
            }
        }"#;
        let info = parse_vulkaninfo(json).unwrap();
        assert_eq!(info.heaps_bytes, vec![100]);
    }

    #[test]
    fn empty_object_yields_defaults_not_none() {
        let info = parse_vulkaninfo("{}").expect("valid (empty) json");
        assert!(info.device_name.is_none());
        assert!(info.driver_name.is_none());
        assert!(info.driver_info.is_none());
        assert!(info.api_version.is_none());
        assert!(info.heaps_bytes.is_empty());
    }

    #[test]
    fn garbage_input_is_none_not_panic() {
        assert!(parse_vulkaninfo("not json at all").is_none());
        assert!(parse_vulkaninfo("").is_none());
        assert!(parse_vulkaninfo("[[[").is_none());
    }

    #[test]
    fn temp_dir_is_exclusive_unique_and_cleaned_up() {
        // First dir: must exist and be a real directory we just created.
        let first = TempDir::new("xrocmtop-vulkaninfo-test").expect("first temp dir");
        let first_path = first.path().to_path_buf();
        assert!(first_path.is_dir(), "temp dir should exist as a directory");

        // Second dir: exclusive `create_dir` + retry must hand out a different path.
        let second = TempDir::new("xrocmtop-vulkaninfo-test").expect("second temp dir");
        let second_path = second.path().to_path_buf();
        assert!(second_path.is_dir());
        assert_ne!(
            first_path, second_path,
            "each TempDir must get a fresh, distinct path"
        );

        // Drop must remove the directory.
        drop(first);
        assert!(!first_path.exists(), "dropped TempDir should be removed");
        drop(second);
        assert!(!second_path.exists(), "dropped TempDir should be removed");
    }

    #[test]
    fn missing_heaps_yields_empty_vec() {
        let json = r#"{ "VkPhysicalDeviceProperties": { "deviceName": "X" } }"#;
        let info = parse_vulkaninfo(json).unwrap();
        assert!(info.heaps_bytes.is_empty());
    }
}
