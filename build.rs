use std::env;

fn main() {
    #[cfg(feature = "gui")]
    tauri_build::build();
    println!("cargo:rerun-if-env-changed=CROSSDESK_CHECK_ONLY");
    if env::var_os("CROSSDESK_CHECK_ONLY").is_some() {
        return;
    }

    println!("cargo:rerun-if-changed=assets/deskbridge.ico");
    println!("cargo:rerun-if-changed=assets/deskbridge-status.png");
    println!("cargo:rerun-if-changed=packaging/windows/deskbridge.rc");
    println!("cargo:rerun-if-changed=src/input/macos_native.c");
    println!("cargo:rerun-if-changed=src/input/macos_native.h");
    println!("cargo:rerun-if-changed=src/macos_status_item.m");

    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("macos")) {
        build_macos_native();
        return;
    }

    #[cfg(feature = "gui")]
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        println!(
            "cargo:warning=Windows executable resources are handled by Tauri; skipping legacy rc embedding to avoid duplicate ICON resources"
        );
    }
}

fn build_macos_native() {
    cc::Build::new()
        .file("src/input/macos_native.c")
        .warnings(true)
        .compile("deskbridge_macos_native");

    #[cfg(feature = "gui")]
    cc::Build::new()
        .file("src/macos_status_item.m")
        .flag("-fobjc-arc")
        .warnings(true)
        .compile("deskbridge_macos_status_item");

    println!("cargo:rustc-link-lib=framework=ApplicationServices");
    println!("cargo:rustc-link-lib=framework=Carbon");
    println!("cargo:rustc-link-lib=framework=IOKit");
    println!("cargo:rustc-link-lib=framework=AppKit");
}
