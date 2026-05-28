// Tell rustc where libkrun lives. brew installs to {brew --prefix}/lib on
// macOS; on Linux it's typically /usr/lib or /usr/local/lib. We ask brew first
// and fall back to /usr/local.

use std::process::Command;

fn main() {
    let prefix = Command::new("brew")
        .args(["--prefix", "libkrun"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "/usr/local".to_string());

    println!("cargo:rustc-link-search=native={prefix}/lib");
    println!("cargo:rustc-link-lib=dylib=krun");
    // Embed an rpath so the binary finds libkrun.dylib at runtime without
    // needing DYLD_LIBRARY_PATH.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}/lib");
    println!("cargo:rerun-if-changed=build.rs");
}
