//! Fingerprints the code that decides what a build writes: the extractor, the
//! history builder and the locked dependency versions. A saved build state is
//! only extended by a binary with the same fingerprint, so one database never
//! mixes the output of two versions of the extractor.

use std::path::{Path, PathBuf};

fn main() {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = crate_dir.join("../..");
    let mut inputs = vec![
        root.join("Cargo.lock"),
        root.join("crates/extract/Cargo.toml"),
        crate_dir.join("Cargo.toml"),
        crate_dir.join("src/build.rs"),
        crate_dir.join("src/generate.rs"),
    ];
    // A directory is rescanned by cargo, so new files are noticed too.
    println!("cargo::rerun-if-changed={}", root.join("crates/extract/src").display());
    let mut extract = Vec::new();
    files_under(&root.join("crates/extract/src"), &mut extract);
    extract.sort();
    inputs.extend(extract);

    // FNV-1a: stable across Rust releases, unlike std's hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &inputs {
        println!("cargo::rerun-if-changed={}", path.display());
        let relative = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().into_owned();
        let content = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for byte in relative.bytes().chain([0]).chain(content).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo::rustc-env=KFH_BUILD_FINGERPRINT={hash:016x}");
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}
