# xrocmtop

[![CI](https://github.com/argakiig/xrocmtop/actions/workflows/ci.yml/badge.svg)](https://github.com/argakiig/xrocmtop/actions/workflows/ci.yml)

A [`btop`](https://github.com/aristocratos/btop)-style terminal UI for monitoring AMD ROCm /
Vulkan GPUs, run entirely from the CLI. Live gauges, scrolling history graphs, a per-process GPU
table, and a Vulkan device panel — for AMD GPUs and APUs, including unified-memory parts.

```
┌ GPU 0 — Radeon 8060S Graphics ──────────────────────────────┐┌ History ──────────────┐
│                          Util 0%                            ││Util 0%                │
│███████████████████████VRAM 46.89 GiB / 96.00 GiB (48%)      ││Power 27 W  ▁▂▃▅▇     │
│██████████             GTT 1.97 GiB / 15.24 GiB (12%)        ││Temp 42°C   ▁▁▂▂▃     │
│Temp 42°C  Power 27.1 W  sclk 2900MHz mclk 937MHz fclk 2000… ││                       │
└─────────────────────────────────────────────────────────────┘└───────────────────────┘
┌ GPU Processes (4, +478 hidden) ─────────────────────────────┐┌ Vulkan ───────────────┐
│PID     Process            VRAM       GTT      GFX  COM      ││ Device  Radeon 8060S… │
│693842  llama-server       27.92 GiB  385 MiB  3%   88%      ││ Driver  radv (Mesa 26…│
│1248555 sd-server          6.32 GiB   130 MiB  41%  n/a      ││ API     1.4.335       │
└─────────────────────────────────────────────────────────────┘└───────────────────────┘
                                              ↑↓ select · Enter detail · s sort:mem · read-only
```

## Why

AMD TUI tooling felt thin. `rocm-smi` is a one-shot text dump, `nvtop` is NVIDIA-only, and the
options that do exist are hard to bend toward the things *I* actually wanted to watch. I set out to
build something I could **own and change** — a focused, hackable, always-on TUI that surfaces the
info I care about, on the hardware I run.

So `xrocmtop` is deliberately:

- **Hackable.** A single small Rust binary, no plugin layer, no config DSL to fight. Each panel is a
  pure function of app state, fixture-tested, and easy to fork: add a column, a panel, or a metric
  without spelunking. The whole thing is built to be edited, not just used.
- **Focused on real signals.** Per-process **VRAM vs GTT** split, per-engine utilization
  (graphics/compute, plus encode/decode in the detail view), full command lines, and the clocks and
  power that actually move — not a wall of registers.
- **APU-first.** It treats unified-memory parts (where "VRAM" is carved from system RAM, alongside a
  separate GTT pool) as a first-class case rather than an afterthought.
- **Honest and unprivileged.** Read-only, no root for the core view, and every unsupported metric
  renders as `n/a` instead of a lie.

## Install

Requires a Rust toolchain (stable). Builds to a single self-contained binary.

```sh
git clone <repo> xrocmtop && cd xrocmtop
cargo build --release
./target/release/xrocmtop
# or install onto your PATH:
cargo install --path .
```

Prebuilt Linux binaries (glibc and static musl) are attached to each
[GitHub Release](https://github.com/argakiig/xrocmtop/releases) — download the tarball for your
target, verify the `.sha256`, and drop `xrocmtop` on your `PATH`.

## Requirements

- Linux with the `amdgpu` driver (reads `/sys/class/drm/cardN/device/...`).
- **No root required** for the core view — all metrics are read as a normal user.
- Optional external tools, each used only if present (the tool degrades gracefully without them):
  - `rocm-smi` — fills static identity (device name, VBIOS, IDs).
  - `vulkaninfo` — populates the Vulkan device panel.

## Usage

```
xrocmtop [OPTIONS]

  -i, --interval <MS>   Refresh interval in milliseconds       [default: 1000]
      --gpu <INDEX>     Restrict the view to a single GPU index
      --no-vulkan       Hide the Vulkan device panel
      --no-procs        Hide per-process GPU accounting
      --history <N>     Samples retained for history graphs     [default: 240]
      --once            Print a single snapshot and exit (no TUI; scriptable)
      --json            With --once, emit JSON instead of text
  -h, --help / -V, --version
```

### Keys

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl-C` | Quit (restores the terminal cleanly) |
| `?` | Toggle the help overlay (lists all keys) |
| `Tab` | Focus the next panel |
| `[` `]` / `←` `→` | Move the focused panel earlier / later in the layout |
| `↑` `↓` / `j` `k` | Select a process row (when the Processes panel is focused) |
| `Enter` | Open the detail popup for the selected process (`Esc` / any key closes) |
| `1` `2` `3` `4` | Toggle the Gauges / Graphs / Processes / Vulkan panel |
| `t` | Cycle the color theme |
| `p` | Pause / resume refreshing |
| `s` | Cycle the process-table sort: memory → pid → name |

Panel layout (order + which are hidden) and the chosen theme are **saved automatically on exit**
and restored next launch.

### Customization

Colors and layout live in `~/.config/xrocmtop/config.toml` (respects `XDG_CONFIG_HOME`). The
file is written for you when you customize at runtime, but you can also hand-edit it. A bad value
is ignored rather than fatal.

```toml
# Built-in preset: "default", "high-contrast", or "mono" (cycle live with `t`).
theme = "default"

# Optional per-element overrides applied on top of the preset.
# Values are named colors (green, cyan, darkgray, lightblue, …) or hex (#ff8800).
[colors]
util_bar = "#00d7af"
vram_bar = "cyan"
gtt_bar  = "blue"
focus    = "lightcyan"   # focused-panel border
accent   = "yellow"      # paused/hidden/table-header
# also: border, title, text, dim, footer, graph_util, graph_power, graph_temp

# Panel order and which are hidden (managed by the toggle/move keys, but editable).
order  = ["gauges", "graphs", "processes", "vulkan"]
hidden = []
```

### Scripting

`--once` makes it a plain reporter — no TUI — for cron jobs, dashboards, or quick checks:

```sh
xrocmtop --once                 # human-readable summary
xrocmtop --once --json          # stable JSON (schema is a supported contract)
xrocmtop --once --json --no-procs --no-vulkan | jq '.gpus[0].mem'
```

## What it shows

- **Per GPU:** utilization, VRAM and GTT (used / total / %), edge temperature, package power, and
  sclk/mclk/fclk/socclk clocks — as live gauges.
- **History graphs:** rolling sparklines for utilization, power, and temperature.
- **Processes:** each process holding an `amdgpu` DRM handle, with its GPU memory split into the
  **VRAM** and **GTT** pools and per-engine utilization (**GFX** and **compute** in the table;
  encode/decode in the detail view). Sorted and sortable; select a row with `↑`/`↓` and press
  `Enter` for a detail popup showing the full command line, every memory pool and engine, and a
  per-`drm-client-id` breakdown. Engine percentages are an average over the ~3 s between process
  walks and read **n/a** until a process has been seen twice. Processes you can't inspect without
  elevation are summarized as "+N hidden".
- **Vulkan:** device name, driver, API version, and device-local memory heaps.

Any metric a given card doesn't support (fan, power cap, `mem_busy_percent`, …) renders as **n/a**
rather than failing.

## Design & safety

- **Read-only.** The tool only *reads* `/sys` and `/proc`; it never writes sysfs and never changes
  clocks, power caps, or fan curves.
- **Tiered data sources:** `amdgpu` sysfs every tick (cheap, no fork), `rocm-smi` at a low cadence
  for static identity, `vulkaninfo` once at startup.
- Single binary, single-threaded refresh loop, low idle overhead.

See [`SPEC.md`](SPEC.md) for the full specification — what xrocmtop is, how it's built, its
architecture, and the criteria for "done".

## Development

```sh
cargo test                                       # unit + integration tests
cargo clippy --all-targets -- -D warnings        # lint (warnings are errors)
cargo fmt --all                                  # format
```

Parsers are tested against fixtures captured from real hardware (under `tests/fixtures/`),
including deliberately degraded inputs to prove graceful "n/a" handling.

CI (GitHub Actions, `.github/workflows/ci.yml`) runs format-check, clippy (warnings as errors),
the test suite, and a release build on every push and pull request. Pushing a `v*` tag triggers
`.github/workflows/release.yml`, which builds stripped glibc + static-musl binaries and publishes
them to a GitHub Release.

## Scope (v1)

AMD/`amdgpu` only; local machine only. Not in scope: NVIDIA/Intel, remote/fleet monitoring,
GPU control (the tool is strictly read-only), and OS packaging. These are deliberate future
directions, not part of v1.

## License

MIT — see [`LICENSE`](LICENSE).
