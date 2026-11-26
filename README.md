# dotOrcA — S00 provisional foundation

Independent automatic video compression project. This is a capability probe and contract scaffold, NOT a compressor.

- Root module: pure Go contracts + benchmark build probe. `CGO_ENABLED=0 go test ./...` / `go vet ./...` / `go build ./...` (set env using native syntax on Windows).
- `gui/`: separate Fyne v2.8.2 module; requires CGO and GCC to compile. `go mod tidy` on first networked build, then commit generated go.mod/go.sum. CI produces the probe plus PE imports, build info and SHA256. CI is not a release pipeline.
- `docs/S00.md`: technical audit, module plan and gates.
- `docs/ROADMAP.md`: phase tracking; S00 not accepted yet.

Repository name: dotOrcA; private. License pending user confirmation. No actual FFmpeg invocation, bitrate calculation, benchmark, scheduling or prediction implemented.
