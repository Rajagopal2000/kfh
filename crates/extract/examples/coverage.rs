//! Compares extracted definitions against a style heuristic: kernel function
//! bodies open with `{` alone in column 0. Reports overall recall and the
//! files with the most misses.
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let rel = p.to_string_lossy();
        if ["/tools", "/scripts", "/Documentation", "/samples", "/usr"]
            .iter()
            .any(|d| rel.ends_with(d))
        {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if rel.ends_with(".c") || rel.ends_with(".h") {
            out.push(p);
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: coverage <kernel-tree>");
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    let (mut expected, mut found) = (0usize, 0usize);
    let mut misses = Vec::new();
    for f in &files {
        let src = std::fs::read(f).unwrap();
        let text = String::from_utf8_lossy(&src);
        let braces = text.lines().filter(|l| *l == "{").count();
        let n = extract::extract(&src).len();
        expected += braces;
        found += n.min(braces);
        if braces > n {
            misses.push((braces - n, f.display().to_string()));
        }
    }
    misses.sort_by(|a, b| b.cmp(a));
    println!(
        "files {}  expected {}  found {}  recall {:.2}%",
        files.len(),
        expected,
        found,
        100.0 * found as f64 / expected as f64
    );
    for (m, f) in misses.iter().take(25) {
        println!("{m:>5} {f}");
    }
}
