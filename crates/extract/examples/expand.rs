//! Expands macro invocations against every macro defined in a kernel tree.
//!
//!   expand <tree> <file> <line> 'TEXT'   expand TEXT as if written at that line, print the union
//!   expand <tree> --stats                         expand every top-level invocation in the tree
use extract::cpp;
use extract::scope::Scope;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
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
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let mut files = Vec::new();
    walk(&root, &mut files);
    let facts: Vec<(Arc<str>, extract::FileFacts)> = files
        .par_iter()
        .map(|f| {
            let rel = f.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
            (Arc::from(rel), extract::analyze(&std::fs::read(f).unwrap()))
        })
        .collect();
    let refs: Vec<(Arc<str>, &extract::FileFacts)> = facts.iter().map(|(p, f)| (p.clone(), f)).collect();
    let scope = Scope::new(&refs);
    eprintln!("{} files", files.len());

    if args[2] != "--stats" {
        // expand <tree> <file> <line> <text>
        let file = scope.file_id(&args[2]).expect("file in tree");
        let line: u32 = args[3].parse().unwrap();
        let u = cpp::expand_union(&args[4], &scope.at(file, line));
        println!(
            "complete: {}, exhaustive: {}, variants: {}, problems: {:?}",
            u.complete,
            u.exhaustive,
            u.variants.len(),
            u.problems
        );
        for v in &u.variants {
            println!(
                "\n== via {:?}\n{}",
                v.origins
                    .iter()
                    .map(|(m, d)| format!("{m}@{}", d.location()))
                    .collect::<Vec<_>>(),
                cpp::render_for_extraction(&v.tokens)
            );
            let (kept, rejected) = extract::functions_in_expansion(&v.tokens);
            for f in kept {
                println!(
                    "  -> {} {}({})",
                    f.return_type,
                    f.name,
                    f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>().join(", ")
                );
            }
            for r in rejected {
                println!("  xx rejected {r}");
            }
        }
        return;
    }

    // Per invocation: (macro, complete, generated names, exhaustive, rejected names).
    type Outcome = (String, bool, Vec<String>, bool, Vec<String>);
    let results: Vec<Outcome> = facts
        .par_iter()
        .enumerate()
        .flat_map_iter(|(id, (_, f))| {
            let scope = &scope;
            f.invocations
                .iter()
                .filter(|inv| scope.is_macro(&inv.name))
                .map(move |inv| {
                    let mut text = inv.text.clone();
                    if inv.followed_by_brace {
                        text.push_str(" { }");
                    }
                    let u = cpp::expand_union(&text, &scope.at(id, inv.line));
                    let mut names = std::collections::BTreeSet::new();
                    let mut rejected = std::collections::BTreeSet::new();
                    if u.complete {
                        for v in &u.variants {
                            let (kept, rej) = extract::functions_in_expansion(&v.tokens);
                            names.extend(kept.into_iter().map(|g| g.name));
                            rejected.extend(rej);
                        }
                    }
                    (
                        inv.name.clone(),
                        u.complete,
                        names.into_iter().collect(),
                        u.exhaustive,
                        rejected.into_iter().collect(),
                    )
                })
        })
        .collect();
    let invocations: usize = facts.iter().map(|(_, f)| f.invocations.len()).sum();
    let incomplete = results.iter().filter(|r| !r.1).count();
    let generated: usize = results.iter().map(|r| r.2.len()).sum();
    let partial = results.iter().filter(|r| r.1 && !r.3).count();
    println!(
        "invocations {invocations}, macro invocations {}, incomplete {incomplete}, not exhaustive {partial}, functions generated {generated}",
        results.len()
    );
    let mut by_macro: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    let rejected: usize = results.iter().map(|r| r.4.len()).sum();
    println!("rejected names {rejected}");
    let mut rej_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for r in &results {
        for n in &r.4 {
            rej_by.entry(&r.0).or_default().push(n);
        }
    }
    let mut rej_top: Vec<_> = rej_by.iter().collect();
    rej_top.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (m, names) in rej_top.iter().take(15) {
        println!("  {:>6} {m}: e.g. {}", names.len(), names[0]);
    }
    for (name, complete, names, _, _) in &results {
        let e = by_macro.entry(name).or_default();
        e.0 += 1;
        e.1 += usize::from(!complete);
        e.2 += names.len();
    }
    let mut top: Vec<_> = by_macro.iter().collect();
    top.sort_by_key(|(_, v)| std::cmp::Reverse(v.2));
    println!("\nmost generated:");
    for (name, (n, inc, g)) in top.iter().take(30) {
        println!("{g:>7} {name} ({n} uses, {inc} incomplete)");
    }
    top.sort_by_key(|(_, v)| std::cmp::Reverse(v.1));
    println!("\nmost incomplete:");
    for (name, (n, inc, g)) in top.iter().take(20) {
        println!("{inc:>7} {name} ({n} uses, {g} generated)");
    }
}
