//! Builds the macOS helper (`macos/`, Swift) for the target being compiled and hands its path and SHA-256 to the
//! crate, which embeds it. Other targets embed nothing: computer use is macOS only.
//!
//! Set `HAARNESS_SKIP_MAC_HELPER=1` to build without the helper (computer use then reports that this build lacks
//! it). A failed Swift build otherwise fails the build, so a release never ships without the helper by accident.
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let target = out.join("haarness-mac");
    println!("cargo:rerun-if-changed=macos/Sources");
    println!("cargo:rerun-if-env-changed=HAARNESS_SKIP_MAC_HELPER");
    let macos = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let skip =
        env::var("HAARNESS_SKIP_MAC_HELPER").is_ok_and(|value| !value.is_empty() && value != "0");
    if !macos || skip {
        fs::write(&target, b"").expect("write empty helper");
        export(&target, "");
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("computer use: no macOS helper for architecture {other:?}"),
    };
    let triple = format!("{arch}-apple-macosx12.0");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let sources = manifest.join("macos/Sources");
    let kit = out.join("mac-kit");
    fs::create_dir_all(&kit).expect("create mac-kit");
    // The helper reports the Haarness version it was built with; the checked-in Build.swift says "dev".
    let stamp = kit.join("Build.swift");
    fs::write(
        &stamp,
        format!(
            "let build = {:?}\n",
            env::var("CARGO_PKG_VERSION").unwrap_or_default()
        ),
    )
    .expect("write Build.swift");
    let mut kit_sources = swift_files(&sources.join("MacKit"));
    kit_sources.retain(|path| path.file_name().is_none_or(|name| name != "Build.swift"));
    kit_sources.push(stamp);
    let helper_sources = swift_files(&sources.join("MacHelper"));
    for file in kit_sources.iter().chain(&helper_sources) {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    let profile = if env::var("PROFILE").as_deref() == Ok("release") {
        "-O"
    } else {
        "-Onone"
    };
    run(Command::new("xcrun")
        .args(["swiftc", profile, "-swift-version", "5", "-target", &triple])
        .args([
            "-parse-as-library",
            "-module-name",
            "MacKit",
            "-emit-module",
            "-emit-module-path",
        ])
        .arg(kit.join("MacKit.swiftmodule"))
        .args(["-emit-library", "-static", "-o"])
        .arg(kit.join("libMacKit.a"))
        .args(&kit_sources));
    run(Command::new("xcrun")
        .args([
            "swiftc",
            profile,
            "-swift-version",
            "5",
            "-target",
            &triple,
            "-module-name",
            "HaarnessMac",
        ])
        .arg("-I")
        .arg(&kit)
        .arg("-L")
        .arg(&kit)
        .arg("-lMacKit")
        .args(&helper_sources)
        .arg("-o")
        .arg(&target));
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&target));
    let bytes = fs::read(&target).expect("read helper");
    export(&target, &hex::encode(Sha256::digest(&bytes)));
}

fn export(path: &std::path::Path, sha: &str) {
    println!(
        "cargo:rustc-env=HAARNESS_MAC_HELPER_PATH={}",
        path.display()
    );
    println!("cargo:rustc-env=HAARNESS_MAC_HELPER_SHA={sha}");
}

fn swift_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "swift")
        })
        .collect();
    files.sort();
    files
}

fn run(command: &mut Command) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("computer use: cannot run {command:?}: {error}. Install Xcode or the Command Line Tools, or set HAARNESS_SKIP_MAC_HELPER=1."));
    assert!(
        output.status.success(),
        "computer use: building the macOS helper failed ({command:?}):\n{}\n{}\nSet HAARNESS_SKIP_MAC_HELPER=1 to build without it.",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
