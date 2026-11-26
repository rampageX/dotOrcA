# S00 CUI candidate (2026-11-26)

User results: WinSafe and NWG both ~300 KB, acceptable startup, visible counter continues during title-bar drag. GPUI ~16 MB, average startup, unacceptable build duration; user eliminated GPUI. Retain original experiment for evidence only.

CUI v1.5.0 pinned f9597a30d3bba4f7ef3e0f918e31c28bdcd4d2a3. Standalone Rust crate (not resolving GPUI), thin C ABI bridge, identical arena-common worker, explicit software renderer, dark theme, event-driven idle, file drop, system font fallback (no font distribution), independent Windows MSVC CI. CUI library may still link the Windows OpenGL system DLL because upstream includes both renderers; software renderer selection is not a claim of zero PE OpenGL imports. Compare startup, size, CPU, memory, paths, 5s worker and mixed DPI. CUI first-run system font load remains part of measured launch; test CJK glyphs and IME manually. No release, no engine development. Upstream licenses included in CI ZIP. CUI is a young C99 library; FFI and input handling need qualification.

## First Windows CI attempt
Run 45772063429, job 139746831878: CUI and bridge static libraries compiled; Rust compilation reached final MSVC link. LNK2019 for RegCloseKey, RegOpenKeyExW and RegQueryValueExW in cui__plat_system_dark. Added advapi32 to Rust build.rs system libraries. No source/UI change. Rebuild required.
