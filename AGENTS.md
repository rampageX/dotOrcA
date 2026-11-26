# dotOrcA — engineering rules (S00 draft)

## Scope and delivery
- The project charter is authoritative. Only the active S-stage may be implemented. Never mark a stage complete without evidence and user acceptance. No FGS source/dependency inheritance.
- Core and benchmark must build with CGO_ENABLED=0; Fyne is isolated in gui/. GUI does not construct encoder commands or perform heavy work. No runtime PowerShell/BAT/Python/.NET/WebView2; CI scripts are not runtime dependencies.
- Use explicit absolute executable paths and argument arrays through os/exec. Never interpolate a shell command or manually prequote ordinary arguments. Preserve Unicode; test the receiving Windows executable's actual argv, including quote/backslash boundaries. No global cwd changes. Treat untrusted filenames as data, not encoder options or filter expressions.
- Cancellation must terminate the process tree, not merely the parent. S01 must prove Windows Job Object association has no child-escape race, drain both output streams, bound logs, clean owned temporary files only. Preserve source and existing user files. Test disk exhaustion and cancellation.
- Core uses bytes (int64), rational FPS, explicit unknown values, typed decisions and stable reason codes. No estimated metrics presented as measured. No silent HDR->SDR, upscaling, frame interpolation, overwrite or fallback. Capacity is verified against final muxed file bytes before publication. Infeasible budgets fail explicitly.
- Default hardware route. Software eligibility requires predicted total time <=180s AND speed >=1x AND credible quality/efficiency gain, except explicit hardware-unavailable fallback policy to be decided in S05. Unknown predictions must not justify discretionary software encoding.
- Keep official dataset, local observations and configuration separate; version schemas; atomic writes and safe migrations. No overwrite of local learning on update. No automatic telemetry without explicit opt-in. Benchmark data records tool versions, settings, hardware, driver, source identity, actual runtime, bytes and measured quality method.
- Hardware capabilities cached by device/tool/driver fingerprint; invalidate on change, not UI interaction. All GUI changes from worker goroutines use fyne.Do; never block UI.

## Regression requirements
- Mandatory exact basename: `小K 的鹅腿坟场 & (黑洞) ^ % !.avi`. Also spaces, non-BMP Unicode, long paths, UNC, leading hyphens, file URI decoding; argv echo helper (real Windows subprocess), not merely string serialization.
- Add automated regression before a defect fix can be accepted. Cover success/error/cancel, missing tools, disk exhaustion, subprocess tree cleanup, large file sizes, extreme duration, mux timestamps, audio sync, HDR tags/metadata, configuration compatibility and UI responsiveness. S00 only creates contracts and a GUI probe; later stages must implement applicable tests before acceptance.
- `gofmt`, `go vet`, `go test -count=1`, and pure-Go builds in CI. Native GUI build and PE import audit on Windows; GUI and hardware smoke on actual Windows. Never label CI compilation as real-device validation.

## Release gate
- Pin Go patch, module checksums and toolchain/action revisions after the bootstrap; no floating dependency upgrades in release jobs. Commit go.mod/go.sum, build identity, provenance, dependency/license inventory and SHA256. Check PE imports against approved OS DLL list and clean Windows host launch (no MinGW installed).
- No tag/release until required CI and manual acceptance complete; build from exact reviewed commit, publish checksums, retain evidence. Release artifacts only from CI; no secret commits, no unrelated source edits, no public release or license assumption before user confirmation.
- Use LF for text via .gitattributes; binary assets explicitly marked. Keep stage card and evidence current. No placeholder passing tests for unimplemented behavior.
