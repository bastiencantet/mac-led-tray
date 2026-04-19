use std::path::PathBuf;

fn main() {
    // Only link Sparkle into the main tray binary, not the helper.
    // We key off CARGO_BIN_NAME: only "mac-led-tray" needs Sparkle.
    let bin_name = std::env::var("CARGO_BIN_NAME").unwrap_or_default();
    if bin_name != "mac-led-tray" {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let framework_dir = manifest_dir.join("vendor");
    assert!(
        framework_dir.join("Sparkle.framework").exists(),
        "Sparkle.framework not found at {}",
        framework_dir.display()
    );

    // Link against Sparkle at build time, look for it in vendor/ during link.
    println!(
        "cargo:rustc-link-search=framework={}",
        framework_dir.display()
    );
    println!("cargo:rustc-link-lib=framework=Sparkle");

    // At runtime, resolve @rpath to the app's embedded Frameworks directory.
    // bundle.sh copies Sparkle.framework into LED.app/Contents/Frameworks/.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");

    println!("cargo:rerun-if-changed=vendor/Sparkle.framework");
    println!("cargo:rerun-if-changed=build.rs");
}
