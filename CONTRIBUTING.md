# Contributing to xrocmtop

Thanks for your interest. `xrocmtop` is a deliberately small, hackable Rust binary —
contributions that keep it that way are very welcome.

## Ground rules

This tool is **read-only and unprivileged** by design. The full set of boundaries lives
in [`SPEC.md`](SPEC.md); the ones that most affect contributions:

**Always**
- Treat every metric as optional — render `n/a` and keep going when a source is
  absent or unsupported.
- Read-only access to `/sys` and `/proc`. Capture a real fixture when adding any new
  parser (see [Fixtures](#fixtures)).

**Ask first** (open an issue before sending a PR)
- Adding a dependency beyond the current set (especially `ash` / a Vulkan loader or an
  async runtime).
- Changing the refresh architecture (single-loop vs. threads, per-source cadence).
- Adding a non-AMD vendor backend or any networked/remote feature.
- Changing `--json` output beyond additive fields — the schema is a tested contract.

**Never**
- Write to sysfs or otherwise *control* the GPU (no clock/power/fan changes, no
  signalling or killing processes). This tool only reads.
- Require root for core operation, or panic on a missing/unsupported metric.
- Remove or skip failing tests to go green; commit secrets or credentials.

## Development

```sh
cargo build           # debug build
cargo run             # run the TUI
cargo run -- --once   # single text snapshot (scriptable)
```

Before every commit, run the same gate CI enforces:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

All three must pass clean — CI runs `fmt --check`, `clippy -D warnings`, `test`, and a
release build on every push and PR.

## Fixtures

Parsers are the core risk, so they are tested against real captures committed under
`tests/fixtures/` (sysfs, fdinfo, `rocm-smi.json`, `vulkaninfo.json`, `once.json`). When
you add or change a parser:

1. Capture a **real** sample from actual hardware — don't hand-write one.
2. Add the fixture and a unit test next to the code it covers (`#[cfg(test)]`).
3. Include a **degraded** case (missing file, empty field, "Not supported", missing
   engine lines) — degradation must never panic.

UI panels are tested via ratatui's `TestBackend`; the `--once --json` contract is
asserted against `tests/fixtures/once.json` in `tests/once_contract.rs`.

## Pull requests

- Keep PRs focused; one logical change per PR.
- Use clear, conventional-style commit subjects (e.g. `feat(ui): ...`, `fix(collect): ...`).
- Note the hardware you tested on — GPU model, driver/kernel version — since amdgpu
  field names and units vary across cards and kernels.
- Make sure `fmt` / `clippy` / `test` pass before opening the PR.

## License

By contributing, you agree your contributions are licensed under the
[MIT License](LICENSE) that covers this project.
