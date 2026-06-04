//! Per-process amdgpu accounting from `/proc/<pid>/fdinfo/<fd>`.
//!
//! Every process that holds an open amdgpu DRM handle (a render/compute context, a Vulkan
//! swapchain, an X/Wayland compositor, …) exposes one fdinfo file per file descriptor. Each file
//! reports the memory that DRM client is using and cumulative per-engine busy time. We walk
//! `/proc`, keep only amdgpu clients, and aggregate them into one [`ProcInfo`] per pid.
//!
//! Two robustness facts drive the design:
//!
//! - A pid usually has the *same* DRM client open on several fds (e.g. fd 4 and fd 5 both report
//!   `drm-client-id: 196416` with identical numbers). Summing per-fd would multiply memory by the
//!   fd count, so we de-duplicate by `(pid, drm-client-id)` before aggregating.
//! - `/proc` is racy and partly privileged: pids vanish mid-walk and fdinfo of other users' GPU
//!   processes is unreadable without elevation. We never error on that — unreadable pids are
//!   skipped and counted as `hidden`, surfaced in the UI as "+N hidden (needs elevation)".
//!
//! Parsing ([`parse_fdinfo`]) is pure over `&str` and fixture-tested; only [`collect`] /
//! [`collect_in`] touch the filesystem (read-only).
//!
//! ## gfx % — None for v1
//! `drm-engine-gfx` is a *cumulative* nanosecond counter. A utilization percentage requires two
//! samples and the wall-clock delta between them (`Δengine_ns / Δwall_ns`). A single fdinfo read
//! cannot yield that, and this collector is intentionally single-pass and stateless, so
//! [`ProcInfo::gfx_pct`] is left `None`. Per-process memory attribution — the must-have — is fully
//! computed. Engine time is still parsed (and exposed via [`FdGpuUsage::gfx_ns`]) so a future
//! stateful sampler can diff it.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::model::{EngineNs, Opt, ProcClient, ProcInfo};

/// GPU usage extracted from a single amdgpu fdinfo file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FdGpuUsage {
    /// `drm-client-id` — identifies the DRM client so duplicate fds onto the same context can be
    /// de-duplicated within a pid.
    pub client_id: u64,
    /// `drm-memory-vram` in bytes (converted from the file's KiB), or `None` if absent.
    pub vram_bytes: Opt<u64>,
    /// `drm-memory-gtt` in bytes, or `None` if absent.
    pub gtt_bytes: Opt<u64>,
    /// Cumulative `drm-engine-gfx` nanoseconds, or `None` when the line is absent. The percentage
    /// is derived later by [`crate::app::App`]'s sampler from two consecutive walks.
    pub gfx_ns: Opt<u64>,
    /// Cumulative `drm-engine-compute` nanoseconds, or `None` when absent.
    pub compute_ns: Opt<u64>,
    /// Cumulative `drm-engine-enc` (video encode) nanoseconds, or `None` when absent.
    pub enc_ns: Opt<u64>,
    /// Cumulative `drm-engine-dec` (video decode) nanoseconds, or `None` when absent.
    pub dec_ns: Opt<u64>,
}

impl FdGpuUsage {
    /// VRAM + GTT, treating each missing side as zero. `None` only if *both* are absent.
    fn mem_bytes(&self) -> Opt<u64> {
        match (self.vram_bytes, self.gtt_bytes) {
            (None, None) => None,
            (v, g) => Some(v.unwrap_or(0).saturating_add(g.unwrap_or(0))),
        }
    }
}

/// Accumulate an optional addend into an optional running total. The result is `None` only while
/// *no* term has appeared; once any side is present it contributes (missing terms count as zero).
/// Saturates rather than wrapping on hostile inputs. Used to sum memory and cumulative engine ns
/// across a pid's DRM clients.
fn add_opt(acc: Opt<u64>, add: Opt<u64>) -> Opt<u64> {
    match (acc, add) {
        (a, None) => a,
        (None, b @ Some(_)) => b,
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
    }
}

/// Parse one fdinfo file's contents. Returns `None` unless this is an amdgpu DRM client with a
/// usable `drm-client-id` — non-amdgpu drivers (i915, …) and non-DRM fds are rejected so callers
/// can blindly feed every fdinfo file.
pub fn parse_fdinfo(content: &str) -> Option<FdGpuUsage> {
    let mut is_amdgpu = false;
    let mut client_id: Opt<u64> = None;
    let mut vram_bytes: Opt<u64> = None;
    let mut gtt_bytes: Opt<u64> = None;
    let mut gfx_ns: Opt<u64> = None;
    let mut compute_ns: Opt<u64> = None;
    let mut enc_ns: Opt<u64> = None;
    let mut dec_ns: Opt<u64> = None;

    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "drm-driver" => is_amdgpu = value == "amdgpu",
            "drm-client-id" => client_id = value.parse().ok(),
            "drm-memory-vram" => vram_bytes = parse_kib_bytes(value),
            "drm-memory-gtt" => gtt_bytes = parse_kib_bytes(value),
            "drm-engine-gfx" => gfx_ns = parse_ns(value),
            "drm-engine-compute" => compute_ns = parse_ns(value),
            "drm-engine-enc" => enc_ns = parse_ns(value),
            "drm-engine-dec" => dec_ns = parse_ns(value),
            _ => {}
        }
    }

    if !is_amdgpu {
        return None;
    }
    Some(FdGpuUsage {
        client_id: client_id?,
        vram_bytes,
        gtt_bytes,
        gfx_ns,
        compute_ns,
        enc_ns,
        dec_ns,
    })
}

/// Parse a `"<N> KiB"` memory value into bytes. Tolerant of the unit being absent.
fn parse_kib_bytes(value: &str) -> Opt<u64> {
    let kib: u64 = value.split_whitespace().next()?.parse().ok()?;
    // Hostile/garbage fdinfo could report a huge KiB count; refuse to wrap on overflow.
    kib.checked_mul(1024)
}

/// Parse a `"<N> ns"` engine value into nanoseconds.
fn parse_ns(value: &str) -> Opt<u64> {
    value.split_whitespace().next()?.parse().ok()
}

/// Walk the real `/proc` and aggregate amdgpu usage per process.
///
/// Returns the per-process rows plus the number of pids that *looked* like GPU users but whose
/// fdinfo could not be read (typically other users' processes needing elevation).
pub fn collect() -> (Vec<ProcInfo>, usize) {
    collect_in(Path::new("/proc"))
}

/// Like [`collect`] but rooted at an arbitrary `/proc`-shaped tree, so it can be driven against a
/// fixture directory in tests. Rows are returned sorted by memory descending.
pub fn collect_in(proc_root: &Path) -> (Vec<ProcInfo>, usize) {
    let mut rows: Vec<ProcInfo> = Vec::new();
    let mut hidden = 0usize;

    let Ok(entries) = fs::read_dir(proc_root) else {
        return (rows, hidden);
    };

    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue; // non-pid entry like /proc/self, /proc/meminfo
        };
        let pid_dir = entry.path();

        match scan_pid(&pid_dir) {
            PidScan::Gpu {
                mem_bytes,
                vram_bytes,
                gtt_bytes,
                clients,
                engine_ns,
                partial,
            } => {
                let name = read_comm(&pid_dir).unwrap_or_else(|| pid.to_string());
                let cmdline = read_cmdline(&pid_dir);
                rows.push(ProcInfo {
                    pid,
                    name,
                    cmdline,
                    mem_bytes,
                    vram_bytes,
                    gtt_bytes,
                    // Percentages are filled in by the sampler from the raw `engine_ns` counters.
                    gfx_pct: None,
                    compute_pct: None,
                    enc_pct: None,
                    dec_pct: None,
                    clients,
                    engine_ns,
                });
                // A partially-readable GPU pid is still shown (with whatever memory we could
                // read), but it also had unreadable sibling fds whose memory we missed — so it
                // contributes to the "+N hidden" signal too. No double-count: a pid is either a
                // shown Gpu row (possibly +1 hidden here) or a Hidden pid below, never both.
                if partial {
                    hidden += 1;
                }
            }
            PidScan::Hidden => hidden += 1,
            PidScan::NoGpu => {}
        }
    }

    // Memory desc, then pid asc for a stable order when two processes tie (or both lack memory).
    rows.sort_by(|a, b| {
        b.mem_bytes
            .cmp(&a.mem_bytes)
            .then_with(|| a.pid.cmp(&b.pid))
    });
    (rows, hidden)
}

/// Outcome of inspecting one pid's `fdinfo` directory.
enum PidScan {
    /// At least one amdgpu client found; aggregated usage.
    Gpu {
        mem_bytes: Opt<u64>,
        vram_bytes: Opt<u64>,
        gtt_bytes: Opt<u64>,
        /// Per-DRM-client memory breakdown, one entry per unique `drm-client-id`.
        clients: Vec<ProcClient>,
        /// Cumulative engine counters summed across the pid's clients; the sampler turns these
        /// into percentages.
        engine_ns: EngineNs,
        /// True when this pid *also* had unreadable sibling fds, so the readable memory is a
        /// lower bound. The caller surfaces this in the hidden count (see [`collect_in`]).
        partial: bool,
    },
    /// The fdinfo directory exists but could not be read (permission denied) — count as hidden.
    Hidden,
    /// No amdgpu clients (or no fdinfo at all) — ignore.
    NoGpu,
}

/// Inspect `<pid>/fdinfo`, de-duplicating DRM clients and summing their memory and engine time.
fn scan_pid(pid_dir: &Path) -> PidScan {
    let fdinfo_dir = pid_dir.join("fdinfo");
    let Ok(fds) = fs::read_dir(&fdinfo_dir) else {
        // Directory missing → not a GPU process (or pid already gone): ignore.
        // Directory present but unreadable is reported as hidden below.
        return if fdinfo_dir.exists() {
            PidScan::Hidden
        } else {
            PidScan::NoGpu
        };
    };

    let mut seen_clients: HashSet<u64> = HashSet::new();
    let mut clients: Vec<ProcClient> = Vec::new();
    let mut total_mem: Opt<u64> = None;
    let mut total_vram: Opt<u64> = None;
    let mut total_gtt: Opt<u64> = None;
    let mut engine = EngineNs::default();
    let mut any_gpu = false;
    let mut any_unreadable = false;

    for fd in fds.flatten() {
        let Ok(content) = fs::read_to_string(fd.path()) else {
            any_unreadable = true;
            continue;
        };
        let Some(usage) = parse_fdinfo(&content) else {
            continue;
        };
        any_gpu = true;
        // Same client on multiple fds reports identical figures — count it once.
        if !seen_clients.insert(usage.client_id) {
            continue;
        }
        total_mem = add_opt(total_mem, usage.mem_bytes());
        total_vram = add_opt(total_vram, usage.vram_bytes);
        total_gtt = add_opt(total_gtt, usage.gtt_bytes);
        // Cumulative engine ns sum across clients; a pid driving N contexts uses N contexts' time.
        engine.gfx = add_opt(engine.gfx, usage.gfx_ns);
        engine.compute = add_opt(engine.compute, usage.compute_ns);
        engine.enc = add_opt(engine.enc, usage.enc_ns);
        engine.dec = add_opt(engine.dec, usage.dec_ns);
        clients.push(ProcClient {
            client_id: usage.client_id,
            vram_bytes: usage.vram_bytes,
            gtt_bytes: usage.gtt_bytes,
        });
    }

    if any_gpu {
        // Engine percentages are None here — a single-pass collector can't derive a rate. If we
        // found a readable amdgpu client AND some sibling fds were unreadable, keep the pid as a
        // Gpu row (it really is a GPU process and we have *some* memory for it) but flag `partial`
        // so the readable-but-incomplete state is still reflected in the hidden count.
        PidScan::Gpu {
            mem_bytes: total_mem,
            vram_bytes: total_vram,
            gtt_bytes: total_gtt,
            clients,
            engine_ns: engine,
            partial: any_unreadable,
        }
    } else if any_unreadable {
        PidScan::Hidden
    } else {
        PidScan::NoGpu
    }
}

/// Process name from `<pid>/comm`, trimmed. `None` if unreadable/empty.
fn read_comm(pid_dir: &Path) -> Opt<String> {
    let s = fs::read_to_string(pid_dir.join("comm")).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Full command line from `<pid>/cmdline` — NUL-separated args joined with spaces. `None` if
/// unreadable or empty (kernel threads expose an empty cmdline). Read as raw bytes since args are
/// not guaranteed UTF-8; lossily decoded so a stray byte can't drop the whole line.
fn read_cmdline(pid_dir: &Path) -> Opt<String> {
    let raw = fs::read(pid_dir.join("cmdline")).ok()?;
    let joined = raw
        .split(|&b| b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fdinfo")
    }

    fn read_fixture(name: &str) -> String {
        fs::read_to_string(fixtures().join(name)).expect("fixture present")
    }

    #[test]
    fn parses_real_amdgpu_fdinfo() {
        let u = parse_fdinfo(&read_fixture("llama_server.fdinfo")).expect("amdgpu client");
        assert_eq!(u.client_id, 196416);
        assert_eq!(u.vram_bytes, Some(29_971_892 * 1024));
        assert_eq!(u.gtt_bytes, Some(395_148 * 1024));
        assert_eq!(u.gfx_ns, Some(576_304_900));
        assert_eq!(u.compute_ns, Some(25_285_118_630));
        // No video engines on this client.
        assert_eq!(u.enc_ns, None);
        assert_eq!(u.dec_ns, None);
        // VRAM + GTT.
        assert_eq!(u.mem_bytes(), Some((29_971_892 + 395_148) * 1024));
    }

    #[test]
    fn parses_all_four_engines() {
        let u = parse_fdinfo(&read_fixture("video_engines.fdinfo")).expect("amdgpu client");
        assert_eq!(u.gfx_ns, Some(1_000_000_000));
        assert_eq!(u.compute_ns, Some(2_000_000_000));
        assert_eq!(u.enc_ns, Some(3_000_000_000));
        assert_eq!(u.dec_ns, Some(4_000_000_000));
    }

    #[test]
    fn absent_engine_lines_stay_none() {
        // no_engine.fdinfo carries memory but no drm-engine-* lines at all.
        let u = parse_fdinfo(&read_fixture("no_engine.fdinfo")).expect("amdgpu client");
        assert_eq!(u.gfx_ns, None);
        assert_eq!(u.compute_ns, None);
        assert_eq!(u.enc_ns, None);
        assert_eq!(u.dec_ns, None);
    }

    #[test]
    fn parses_memory_without_engine_line() {
        // Must yield memory but gfx_ns None (the deliberately degraded fixture).
        let u = parse_fdinfo(&read_fixture("no_engine.fdinfo")).expect("amdgpu client");
        assert_eq!(u.client_id, 42001);
        assert_eq!(u.vram_bytes, Some(131_072 * 1024));
        assert_eq!(u.gtt_bytes, Some(65_536 * 1024));
        assert_eq!(u.gfx_ns, None);
        assert_eq!(u.mem_bytes(), Some((131_072 + 65_536) * 1024));
    }

    #[test]
    fn rejects_non_amdgpu_driver() {
        assert_eq!(parse_fdinfo(&read_fixture("non_amdgpu.fdinfo")), None);
    }

    #[test]
    fn rejects_plain_non_drm_fd() {
        assert_eq!(parse_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t40\n"), None);
        assert_eq!(parse_fdinfo(""), None);
    }

    #[test]
    fn amdgpu_without_client_id_is_rejected() {
        // No drm-client-id → we can't de-duplicate it safely, so drop it.
        let s = "drm-driver:\tamdgpu\ndrm-memory-vram:\t1024 KiB\n";
        assert_eq!(parse_fdinfo(s), None);
    }

    #[test]
    fn mem_bytes_none_only_when_both_absent() {
        let base = FdGpuUsage {
            client_id: 1,
            ..Default::default()
        };
        assert_eq!(base.mem_bytes(), None);
        assert_eq!(
            FdGpuUsage {
                vram_bytes: Some(10),
                ..base.clone()
            }
            .mem_bytes(),
            Some(10)
        );
        assert_eq!(
            FdGpuUsage {
                gtt_bytes: Some(7),
                ..base.clone()
            }
            .mem_bytes(),
            Some(7)
        );
        assert_eq!(
            FdGpuUsage {
                vram_bytes: Some(10),
                gtt_bytes: Some(7),
                ..base
            }
            .mem_bytes(),
            Some(17)
        );
    }

    // ---- FS walker, against a synthetic /proc tree -------------------------------------------

    /// Build a fake `/proc/<pid>/fdinfo/<fd>` tree under a unique temp dir.
    /// `fds` is a list of (fd_name, fdinfo_fixture_or_literal).
    struct FakeProc {
        root: PathBuf,
    }

    impl FakeProc {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("xrocmtop_proc_{tag}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            FakeProc { root }
        }

        fn add_pid(&self, pid: u32, comm: &str, fds: &[(&str, &str)]) {
            let pid_dir = self.root.join(pid.to_string());
            let fdinfo = pid_dir.join("fdinfo");
            fs::create_dir_all(&fdinfo).unwrap();
            fs::write(pid_dir.join("comm"), format!("{comm}\n")).unwrap();
            for (fd, content) in fds {
                fs::write(fdinfo.join(fd), content).unwrap();
            }
        }

        /// A pid whose fdinfo dir we cannot enumerate (simulate via a *file* named `fdinfo`,
        /// which `read_dir` rejects with NotADirectory — i.e. present-but-unreadable).
        fn add_unreadable_pid(&self, pid: u32) {
            let pid_dir = self.root.join(pid.to_string());
            fs::create_dir_all(&pid_dir).unwrap();
            fs::write(pid_dir.join("fdinfo"), "blocked").unwrap();
        }

        /// Make one fd file inside a readable fdinfo dir unreadable (mode 0). Used to simulate the
        /// partial-readability case. Returns `false` if it cannot guarantee unreadability (running
        /// as root, where mode 0 is still readable), so the caller can skip the assertion.
        #[cfg(unix)]
        fn chmod_unreadable(&self, pid: u32, fd: &str) -> bool {
            use std::os::unix::fs::PermissionsExt;
            if is_root() {
                return false;
            }
            let path = self.root.join(pid.to_string()).join("fdinfo").join(fd);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            true
        }
    }

    /// Root bypasses file permission bits, so any "unreadable fd" simulation is a no-op there.
    #[cfg(unix)]
    fn is_root() -> bool {
        // SAFETY: getuid is always safe — it only reads the calling process's real uid.
        unsafe { libc_getuid() == 0 }
    }

    // Avoid a dependency on the `libc` crate for a single getuid call.
    #[cfg(unix)]
    unsafe fn libc_getuid() -> u32 {
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        unsafe { getuid() }
    }

    impl Drop for FakeProc {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn aggregates_dedupes_and_sorts() {
        let llama = read_fixture("llama_server.fdinfo"); // vram 29971892 + gtt 395148 KiB
        let sd = read_fixture("sd_server.fdinfo"); // vram 6789560 + gtt 133544 KiB
        let fake = FakeProc::new("agg");
        // Big GPU user, same client on two fds → must NOT double-count.
        fake.add_pid(645226, "llama-server", &[("4", &llama), ("5", &llama)]);
        // Smaller GPU user.
        fake.add_pid(1248555, "sd-server", &[("4", &sd)]);
        // Non-pid / non-GPU noise that must be ignored.
        fake.add_pid(999, "bash", &[("0", "pos:\t0\nflags:\t02\n")]);

        let (rows, hidden) = collect_in(&fake.root);
        assert_eq!(hidden, 0);
        assert_eq!(rows.len(), 2, "only the two GPU pids");
        // Sorted memory desc → llama first.
        assert_eq!(rows[0].pid, 645226);
        assert_eq!(rows[0].name, "llama-server");
        assert_eq!(rows[0].mem_bytes, Some((29_971_892 + 395_148) * 1024));
        assert_eq!(rows[0].gfx_pct, None); // v1
        assert_eq!(rows[1].pid, 1248555);
        assert_eq!(rows[1].mem_bytes, Some((6_789_560 + 133_544) * 1024));
    }

    #[test]
    fn counts_unreadable_pids_as_hidden() {
        let sd = read_fixture("sd_server.fdinfo");
        let fake = FakeProc::new("hidden");
        fake.add_pid(100, "sd-server", &[("4", &sd)]);
        fake.add_unreadable_pid(200);
        fake.add_unreadable_pid(300);

        let (rows, hidden) = collect_in(&fake.root);
        assert_eq!(rows.len(), 1);
        assert_eq!(hidden, 2, "two unreadable pids counted, not errored");
    }

    #[test]
    fn missing_proc_root_is_empty_not_error() {
        let (rows, hidden) = collect_in(Path::new("/nonexistent/xrocmtop/proc"));
        assert!(rows.is_empty());
        assert_eq!(hidden, 0);
    }

    #[test]
    fn parse_kib_bytes_overflow_returns_none() {
        // u64::MAX KiB would overflow when multiplied by 1024 — must yield None, not wrap.
        assert_eq!(parse_kib_bytes(&format!("{} KiB", u64::MAX)), None);
        // A value that fits is still parsed.
        assert_eq!(parse_kib_bytes("1024 KiB"), Some(1024 * 1024));
    }

    #[test]
    fn distinct_clients_on_one_pid_sum_not_dedupe() {
        // Two *different* drm-client-ids on one pid → memory must SUM (de-dup is per client-id,
        // not per pid). Distinct from `aggregates_dedupes_and_sorts`, which uses the same client.
        let client_a =
            "drm-driver:\tamdgpu\ndrm-client-id:\t111\ndrm-memory-vram:\t1000 KiB\ndrm-memory-gtt:\t0 KiB\n";
        let client_b =
            "drm-driver:\tamdgpu\ndrm-client-id:\t222\ndrm-memory-vram:\t2000 KiB\ndrm-memory-gtt:\t0 KiB\n";
        let fake = FakeProc::new("distinct");
        fake.add_pid(7000, "multi", &[("4", client_a), ("5", client_b)]);

        let (rows, hidden) = collect_in(&fake.root);
        assert_eq!(hidden, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mem_bytes, Some((1000 + 2000) * 1024));
    }

    #[test]
    fn partial_readable_gpu_pid_is_shown_and_counted_hidden() {
        let sd = read_fixture("sd_server.fdinfo");
        let fake = FakeProc::new("partial");
        // One readable amdgpu fd plus a sibling fd we'll make unreadable.
        fake.add_pid(8000, "partial-gpu", &[("4", &sd), ("5", "placeholder")]);
        if !fake.chmod_unreadable(8000, "5") {
            eprintln!("skipping partial-readability assertion: running as root");
            return;
        }

        let (rows, hidden) = collect_in(&fake.root);
        // Still a shown GPU row with the memory we *could* read.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 8000);
        assert_eq!(rows[0].mem_bytes, Some((6_789_560 + 133_544) * 1024));
        // ...but its unreadable sibling bumps the hidden count (lower-bound memory surfaced).
        assert_eq!(hidden, 1, "partial-readability surfaces as +1 hidden");
    }

    #[test]
    fn per_client_breakdown_and_engine_sum() {
        // Two distinct clients on one pid: memory must break down per-client and engine ns sum.
        let a = "drm-driver:\tamdgpu\ndrm-client-id:\t111\ndrm-memory-vram:\t1000 KiB\ndrm-memory-gtt:\t0 KiB\ndrm-engine-gfx:\t100 ns\ndrm-engine-compute:\t50 ns\n";
        let b = "drm-driver:\tamdgpu\ndrm-client-id:\t222\ndrm-memory-vram:\t2000 KiB\ndrm-memory-gtt:\t0 KiB\ndrm-engine-gfx:\t300 ns\n";
        let fake = FakeProc::new("breakdown");
        fake.add_pid(7100, "multi", &[("4", a), ("5", b)]);

        let (rows, _) = collect_in(&fake.root);
        let p = &rows[0];
        assert_eq!(p.vram_bytes, Some(3000 * 1024));
        assert_eq!(p.clients.len(), 2, "one entry per distinct client");
        // Engine ns summed across clients; compute present on only one client still surfaces.
        assert_eq!(p.engine_ns.gfx, Some(400));
        assert_eq!(p.engine_ns.compute, Some(50));
        assert_eq!(p.engine_ns.enc, None);
        // Collector never derives percentages.
        assert_eq!(p.gfx_pct, None);
        assert_eq!(p.compute_pct, None);
    }

    #[test]
    fn reads_full_cmdline_joined_with_spaces() {
        let sd = read_fixture("sd_server.fdinfo");
        let fake = FakeProc::new("cmdline");
        fake.add_pid(7200, "sd-server", &[("4", &sd)]);
        // cmdline is NUL-separated and NUL-terminated in the kernel.
        fs::write(
            fake.root.join("7200").join("cmdline"),
            b"/usr/bin/sd-server\0--model\0big.gguf\0".as_slice(),
        )
        .unwrap();

        let (rows, _) = collect_in(&fake.root);
        assert_eq!(
            rows[0].cmdline.as_deref(),
            Some("/usr/bin/sd-server --model big.gguf")
        );
    }

    #[test]
    fn missing_cmdline_is_none_not_panic() {
        let sd = read_fixture("sd_server.fdinfo");
        let fake = FakeProc::new("nocmdline");
        fake.add_pid(7300, "sd-server", &[("4", &sd)]);
        // No cmdline file written (mirrors a kernel thread / vanished pid).
        let (rows, _) = collect_in(&fake.root);
        assert_eq!(rows[0].cmdline, None);
    }

    #[test]
    fn read_comm_falls_back_to_numeric_pid() {
        let sd = read_fixture("sd_server.fdinfo");
        let fake = FakeProc::new("nocomm");
        fake.add_pid(9100, "ignored", &[("4", &sd)]);
        // Remove the comm file so read_comm fails → name must fall back to the pid string.
        fs::remove_file(fake.root.join("9100").join("comm")).unwrap();

        let (rows, _hidden) = collect_in(&fake.root);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "9100");
    }
}
