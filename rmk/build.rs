#[path = "./build_common.rs"]
mod common;

use std::path::Path;
use std::process::Command;
use std::{env, fs};

fn main() {
    // Set the compilation target configuration
    let mut cfgs = common::CfgSet::new();
    common::set_target_cfgs(&mut cfgs);

    println!("cargo:rerun-if-changed=build.rs");

    // VENDOR PATCH (olsk60): fold the keyboard.toml into the build hash so a
    // changed layer layout always reinitializes the stored keymap. The time
    // stamp alone does not change when cargo reuses this script's output (a
    // warm `target/` or a restored CI cache), and a 4-layer map read into a
    // 5-layer firmware puts the old top layer's keys on the new spare layer.
    println!("cargo:rerun-if-env-changed=KEYBOARD_TOML_PATH");
    let keyboard_toml = env::var("KEYBOARD_TOML_PATH").ok().inspect(|path| {
        println!("cargo:rerun-if-changed={path}");
    });

    // Compute build hash and write to constants.rs
    let build_hash = compute_build_hash(keyboard_toml.as_deref());
    let constants = format!(
        "#[allow(clippy::redundant_static_lifetimes)]\npub(crate) const BUILD_HASH: u32 = {build_hash:#010x};\n"
    );

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("constants.rs");
    fs::write(&dest_path, constants).expect("Failed to write constants.rs file");
}
fn compute_build_hash(keyboard_toml: Option<&str>) -> u32 {
    // Get the short hash of the latest Git commit. Use "unknown" if it fails
    let commit_id = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Get and format current local time
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // Combine data and compute CRC32
    let combined = format!("{commit_id}_{now}");
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(combined.as_bytes());
    if let Some(toml) = keyboard_toml.and_then(|path| fs::read(path).ok()) {
        hasher.update(&toml);
    }
    hasher.finalize()
}
