# Changelog

All notable changes to `xrocmtop` are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-04

Initial release: a btop-style terminal UI for monitoring AMD ROCm / Vulkan GPUs.

### Added
- **GPU gauges & history** — per-GPU utilization, VRAM and GTT (used / total / %),
  edge temperature, package power, and sclk/mclk/fclk/socclk clocks, with rolling
  sparkline history for utilization, power, and temperature. Multi-GPU systems stack
  inside the single Gauges/Graphs panel.
- **Per-process table & detail** — one row per amdgpu-DRM process with split VRAM/GTT
  memory and live GFX/compute utilization, sortable by memory / pid / name and
  selectable; `Enter` opens a detail popup with the full command line, every memory
  pool, all four engines (graphics/compute/encode/decode), and a per-`drm-client-id`
  breakdown. Inaccessible processes are summarized as "+N hidden".
- **Vulkan panel** — device name, driver, API version, and device-local memory heaps
  parsed from `vulkaninfo --json`.
- **Customization & persistence** — built-in `default`, `high-contrast`, and `mono`
  themes cycled at runtime; `$XDG_CONFIG_HOME/xrocmtop/config.toml` color overrides;
  runtime panel toggle/reorder/focus. Theme, color overrides, and panel layout
  auto-save on exit and reload at startup.
- **Scriptable snapshot** — `--once` prints a single text snapshot; `--once --json`
  emits a parseable JSON document whose field names are a tested public contract.
- **Graceful degradation** — every metric is optional; unsupported metrics render as
  `n/a` and never panic. The terminal is restored via a panic hook + RAII guard.
- **Distribution** — release workflow builds glibc-dynamic and musl-static Linux
  binaries with `.sha256` checksums attached to each GitHub Release.

[Unreleased]: https://github.com/argakiig/xrocmtop/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/argakiig/xrocmtop/releases/tag/v0.1.0
