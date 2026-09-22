use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};
const VERSION: &str = "0.5.9";
fn main() {
    println!("cargo:rerun-if-env-changed=HAARNESS_OPENTUI_ARCHIVE");
    let target = env::var("TARGET").expect("Cargo provides TARGET");
    let asset =
        asset_for(&target).unwrap_or_else(|| panic!("unsupported OpenTUI target: {target}"));
    let archive = if let Some(path) = env::var_os("HAARNESS_OPENTUI_ARCHIVE") {
        PathBuf::from(path)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/opentui/artifacts")
            .join(&asset.file)
    };
    println!("cargo:rerun-if-changed={}", archive.display());
    verify_file(&archive, asset.archive_sha);
    let (library_name, library_sha) = library_metadata(&archive);
    println!(
        "cargo:rustc-env=HAARNESS_OPENTUI_ARCHIVE={}",
        archive.display()
    );
    println!(
        "cargo:rustc-env=HAARNESS_OPENTUI_ARCHIVE_SHA={}",
        asset.archive_sha
    );
    println!("cargo:rustc-env=HAARNESS_OPENTUI_LIBRARY_NAME={library_name}");
    println!("cargo:rustc-env=HAARNESS_OPENTUI_LIBRARY_SHA={library_sha}");
    println!("cargo:rustc-env=HAARNESS_OPENTUI_TARGET={}", asset.target);
}
struct Asset {
    target: &'static str,
    file: String,
    archive_sha: &'static str,
}
fn asset_for(target: &str) -> Option<Asset> {
    let (name, sha) = match target {
        "aarch64-apple-darwin" => (
            "darwin-arm64",
            "1555b8b7c2be20770ec1b2a990857139ceb1cfffc47768833f01f00cd9d74d19",
        ),
        "x86_64-apple-darwin" => (
            "darwin-x64",
            "ce55ffcd1bc72a83926dff318d52b37c25507a17adff42001dd23c61553909e5",
        ),
        "aarch64-unknown-linux-gnu" => (
            "linux-arm64",
            "0bb96f19185aec387ab6c62af76978d94d79f1eb58b9b4451a2348f969fd0c00",
        ),
        "x86_64-unknown-linux-gnu" => (
            "linux-x64",
            "2ee460a8c2c27cdcdf788b3c8449b1a94d8199f3e63f906fea9ef0a9de6e8673",
        ),
        "aarch64-unknown-linux-musl" => (
            "linux-arm64-musl",
            "7853b27925f9bbacf196768536fc935b701b977b4947dd79594b41e6485e4bc5",
        ),
        "x86_64-unknown-linux-musl" => (
            "linux-x64-musl",
            "b24372f358414f1dc5c03f7b20138c29adfe73f70ec98765482e2fce18575d02",
        ),
        "aarch64-pc-windows-msvc" => (
            "windows-arm64",
            "1a401d7766fd67bb8b8018273d667f90b0ffd527dabbf8b48967a4630b1f1604",
        ),
        "x86_64-pc-windows-msvc" => (
            "windows-x64",
            "bc7c90afcda12123c71cfaf1192e07a70cc5db734a73e5baf43f20b7ab0c92d4",
        ),
        _ => return None,
    };
    Some(Asset {
        target: name,
        file: format!("opentui-native-v{VERSION}-{name}.zip"),
        archive_sha: sha,
    })
}
fn verify_file(path: &Path, expected: &str) {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let actual = hex::encode(Sha256::digest(bytes));
    assert_eq!(actual, expected, "OpenTUI archive checksum mismatch");
}
fn library_metadata(path: &Path) -> (String, String) {
    let file = fs::File::open(path).expect("OpenTUI archive opens");
    let mut archive = zip::ZipArchive::new(file).expect("valid OpenTUI zip");
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("valid zip entry");
        let name = entry
            .name()
            .rsplit('/')
            .next()
            .unwrap_or(entry.name())
            .to_owned();
        if name.starts_with("libopentui") || name == "opentui.dll" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("library entry reads");
            return (name, hex::encode(Sha256::digest(bytes)));
        }
    }
    panic!("OpenTUI archive contains no native library");
}
