//! Fork fix: rebuilds the embedded assets when a file under them is added,
//! changed or removed. In release builds `RustEmbed` embeds the files it finds
//! when its derive expands, and only those files are tracked, so a new asset
//! never reached a build that reused this crate.
fn main() {
    // Should be kept in sync with the folders `Assets` includes in src/lib.rs.
    println!("cargo:rerun-if-changed=../../app/assets/bundled");
    println!("cargo:rerun-if-changed=../../app/assets/async");
}
