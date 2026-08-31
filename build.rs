// Embeds Info.plist into the binary as a __TEXT,__info_plist section, so
// TCC/Accessibility can identify this raw CLI executable by CFBundleIdentifier
// (a plain codesign --identifier is not enough - see README).
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let plist_path = std::path::Path::new(&manifest_dir).join("Info.plist");
    println!("cargo:rerun-if-changed={}", plist_path.display());
    println!(
        "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist_path.display()
    );
}
