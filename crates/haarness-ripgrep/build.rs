use sha2::{Digest, Sha256};
use std::{env, fs, io::Read, path::PathBuf};

fn main() {
    // Read the manifest directory when the script runs, not when it was
    // compiled: worktrees sharing a target directory reuse this binary.
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .join("../../vendor/ripgrep");
    let manifest_path = root.join("manifest.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(manifest_path).expect("vendored ripgrep manifest"))
            .expect("valid ripgrep manifest");
    let target = env::var("TARGET").expect("Cargo target");
    // Upstream's static musl builds work on both GNU and musl distributions.
    let asset_target = match target.as_str() {
        "aarch64-unknown-linux-gnu" => "aarch64-unknown-linux-musl",
        "x86_64-unknown-linux-gnu" => "x86_64-unknown-linux-musl",
        other => other,
    };
    let asset = &manifest["assets"][asset_target];
    let field = |key: &str| {
        asset[key]
            .as_str()
            .unwrap_or_else(|| panic!("missing ripgrep {key} for {target}"))
    };
    let path = root.join("artifacts").join(field("file"));
    println!("cargo:rerun-if-changed={}", path.display());
    let archive = fs::read(&path).expect("vendored ripgrep payload");
    assert_eq!(
        hex::encode(Sha256::digest(&archive)),
        field("archive_sha256")
    );
    let mut binary = Vec::new();
    flate2::read::GzDecoder::new(archive.as_slice())
        .read_to_end(&mut binary)
        .expect("valid ripgrep gzip payload");
    assert_eq!(hex::encode(Sha256::digest(&binary)), field("binary_sha256"));
    println!("cargo:rustc-env=HAARNESS_RG_ARCHIVE={}", path.display());
    println!("cargo:rustc-env=HAARNESS_RG_SHA={}", field("binary_sha256"));
    println!("cargo:rustc-env=HAARNESS_RG_NAME={}", field("binary"));
    println!("cargo:rustc-env=HAARNESS_RG_TARGET={asset_target}");
    println!(
        "cargo:rustc-env=HAARNESS_RG_VERSION={}",
        manifest["version"].as_str().expect("ripgrep version")
    );
}
