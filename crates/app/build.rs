//! Windows only: embeds the version details and the icon that Explorer, the exe's Properties
//! and Task Manager show. Other platforms have nothing to embed.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy())
        .set("ProductName", "Peeroxide")
        // Task Manager lists the process under this name.
        .set("FileDescription", "Peeroxide")
        .set("CompanyName", "Peeroxide")
        .set(
            "LegalCopyright",
            "Copyright © 2026 Peeroxide contributors. Licensed under the Apache License 2.0.",
        )
        .set("OriginalFilename", "peeroxide.exe")
        .set("InternalName", "peeroxide")
        .set("Comments", "https://github.com/gventino/peeroxide");
    if let Err(e) = res.compile() {
        panic!(
            "could not embed the Windows version details and icon (this needs rc.exe from the \
             Windows SDK, which the MSVC toolchain installs): {e}"
        );
    }
}
