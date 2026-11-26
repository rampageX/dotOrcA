use std::{env, path::PathBuf};
fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = PathBuf::from(env::var_os("CUI_LIB_DIR").expect("CUI_LIB_DIR must point to the CMake Release library directory"));
    for lib in ["cui_bridge.lib", "cui.lib"] {
        assert!(dir.join(lib).is_file(), "missing {}", dir.join(lib).display());
        println!("cargo:rerun-if-changed={}", dir.join(lib).display());
    }
    println!("cargo:rerun-if-env-changed=CUI_LIB_DIR");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=cui_bridge");
    println!("cargo:rustc-link-lib=static=cui");
    for lib in ["user32", "gdi32", "opengl32", "shell32", "imm32", "dwmapi", "comdlg32", "ole32", "advapi32"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    let icon = dir.join("dotorca.res");
    assert!(icon.is_file(), "missing {}", icon.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rustc-link-arg-bin=dotorca={}", icon.display());
    println!("cargo:rustc-link-arg=/OPT:REF");
    println!("cargo:rustc-link-arg=/OPT:ICF");
}
