# Project roadmap — draft V1.0 / 2026-11-26

Original launch charter remains authoritative. Current stage: S00 only.

- [ ] S00: technology audit, module architecture, AGENTS.md, repo + CI initialization (private repo created; Windows CI and user acceptance pending).
- [ ] S01: media parsing, hardware capability discovery, subprocess management.
- [ ] S02: benchmark and standard test corpus.
- [ ] S03: CPU/GPU performance data and prediction.
- [ ] S04: target-size budgeting and quality optimization.
- [ ] S05: encoder decision engine.
- [ ] S06: complete pipeline, size verification and recovery.
- [ ] S07: local learning and official dataset updates.
- [ ] S08: Fyne dark GUI, file drop, batch jobs.
- [ ] S09: real-device performance and compatibility.
- [ ] S10: release.

Product constraints: hard final file size limit, retain quality/HDR/audio sync, target size/codec/max resolution as primary controls, no pointless upscale/frame interpolation. Hardware default, discretionary software only when <=180s, >=1x and actual gain. Distinguish hardware-unavailable policy from discretionary CPU routing. Conservative estimates with missing data. Official and local datasets separately versioned. Windows x64 portable application with external encoders/data, direct os/exec argv, no shell at runtime. Each stage acceptance required.
