# S00-Rust GUI arena
Three candidates (not NWG2 or GPUI Fast). Same release profile, compiler, architecture, worker and basic controls. Dependencies pinned at direct-crate level. Generate lockfiles in first CI; lock them before comparison is finalized. No packing. Native theme completeness is a separate qualification gate, not a free advantage.

Round 1: executable launches, button, status, multiline file paths, clear button, file drop, independent 5-second worker. WinSafe and NWG default native light controls; GPUI uses a dark theme. Do not rank final dark-theme resource use from these baseline builds.

Round 2: full consistent dark theme with accessible contrast, equivalent resize behavior, mixed DPI and normal-user file drop. No undocumented dark-mode APIs without explicit compatibility handling. Inspect PE imports and test without compiler runtime installed. Assess maintenance and implementation complexity (NWG 1.0.13 older release). First paint before background initialization.

## Measurement protocol
Same Windows machine, drive, power plan, display and Defender policy. Run each candidate once from a fresh extraction and record initial visible frame and responsiveness (include first-run scan). Reboot or otherwise control cache conditions for genuine cold launch; do not call a fresh process a cold cache. Alternate candidates across reboots; record 3 cold and 5 warm launches where practical. External high-resolution screen recording or timing harness: process creation to first visible usable window, not just HWND creation or WaitForInputIdle. Record median and worst, exact EXE bytes, idle private bytes/working set and CPU after 15 s, runtime DLL imports, no console. No mandatory Defender disable.
- Drop several files including exact basename 小K 的鹅腿坟场 & (黑洞) ^ % !.avi. No media parsing; empty fixture works.
- Click worker and immediately hold title bar >=8 seconds. Release: DONE 5/5, actual elapsed near 5000 ms (<=6500 ms). UI pause alone is not worker failure.
- Test resizing/minimize/restore, 100–200% and mixed DPI, normal user.
- Compare overall appearance with dark-theme coverage. Startup is primary; size and memory secondary. No fabricated benchmarks.
S00 remains open; no production engine migration yet.

## First CI run (2026-11-26)
- Baseline a5f9a94656dcbdf82ab9820c0f6d0e9d9e79cb7f, PR run 45767407624: GPUI succeeded; NWG E0446 (private Probe in NwgUi public interface); WinSafe E0560 (ButtonOpts uses width/height rather than size). Corrected both source errors, GPUI unchanged. Node.js 20 action deprecation warning is unrelated to the compilation failures. Rebuild pending.
