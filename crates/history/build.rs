//! Fingerprints the code that decides what a build writes: the extractor, the
//! history builder and the locked dependency versions. A saved build state is
//! only extended by a binary with the same fingerprint, so one database never
//! mixes the output of two versions of the extractor.

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    // Paths relative to the workspace root, so the fingerprint does not depend
    // on where the source is checked out.
    let mut inputs: Vec<String> = [
        "Cargo.lock",
        "crates/extract/Cargo.toml",
        "crates/history/Cargo.toml",
        "crates/history/src/build.rs",
        "crates/history/src/generate.rs",
    ]
    .map(String::from)
    .to_vec();
    // A directory is rescanned by cargo, so new files are noticed too.
    println!("cargo::rerun-if-changed={}", root.join("crates/extract/src").display());
    let mut extract = Vec::new();
    files_under(&root, Path::new("crates/extract/src"), &mut extract);
    extract.sort();
    inputs.extend(extract);

    // FNV-1a: stable across Rust releases, unlike std's hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for relative in &inputs {
        let path = root.join(relative);
        println!("cargo::rerun-if-changed={}", path.display());
        let content = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for byte in relative.bytes().chain([0]).chain(content).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo::rustc-env=KFH_BUILD_FINGERPRINT={hash:016x}");
}

/// Files under `root/dir`, as paths relative to `root` with `/` separators.
fn files_under(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let full = root.join(dir);
    for entry in std::fs::read_dir(&full).unwrap_or_else(|e| panic!("reading {}: {e}", full.display())) {
        let name = entry.unwrap().file_name();
        let relative = dir.join(&name);
        if root.join(&relative).is_dir() {
            files_under(root, &relative, out);
        } else {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}
