//! Adding releases to a saved build state must write exactly what a build of
//! every release from scratch writes.

use git2::{Repository, Signature};
use history::build::{Options, build};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Functions the replacement checks expect in every release.
const SENTINELS: &str = "int vfs_read(void) { return 0; }\nint tcp_sendmsg(void) { return 0; }\nvoid schedule(void) {}\nvoid kmem_cache_free(void) {}\n";

/// A repository whose releases each have the given files, plus the sentinels.
fn repo(dir: &Path, releases: &[(&str, &[(&str, &str)])]) {
    let repo = Repository::init_bare(dir).unwrap();
    let who = Signature::now("kfh", "kfh@example.com").unwrap();
    let mut parent = None;
    for (tag, files) in releases {
        let mut tree = repo.treebuilder(None).unwrap();
        for (name, content) in files.iter().chain(&[("sentinels.c", SENTINELS)]) {
            let blob = repo.blob(content.as_bytes()).unwrap();
            tree.insert(name, blob, 0o100644).unwrap();
        }
        let tree = repo.find_tree(tree.write().unwrap()).unwrap();
        let parents: Vec<git2::Commit> = parent.iter().map(|id| repo.find_commit(*id).unwrap()).collect();
        let parents: Vec<&git2::Commit> = parents.iter().collect();
        let commit = repo.commit(None, &who, &who, tag, &tree, &parents).unwrap();
        repo.tag_lightweight(tag, &repo.find_object(commit, None).unwrap(), false)
            .unwrap();
        parent = Some(commit);
    }
}

/// Every row of a database, without row ids.
fn dump(db: &Path) -> Vec<String> {
    let db = Connection::open(db).unwrap();
    let queries = [
        "SELECT 'version', ord, tag FROM versions",
        "SELECT 'span', d.name, d.file, g.kind, g.params, g.attributes, coalesce(g.body, ''),
                s.first_ord, s.last_ord, s.lines
         FROM spans s JOIN defs d ON d.id = s.def_id JOIN signatures g ON g.id = s.sig_id",
        "SELECT 'target', d.name, d.file, t.first_ord, t.last_ord, t.primary_calls, t.other_calls
         FROM macro_targets t JOIN defs d ON d.id = t.def_id",
        "SELECT 'def', name, file FROM defs",
        "SELECT 'sig', kind, return_type, params, attributes, coalesce(body, '') FROM signatures",
    ];
    let mut rows = Vec::new();
    for q in queries {
        let mut stmt = db.prepare(q).unwrap();
        let n = stmt.column_count();
        let mut r = stmt.query([]).unwrap();
        while let Some(row) = r.next().unwrap() {
            let cols: Vec<String> = (0..n)
                .map(|i| match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                    rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    other => format!("{other:?}"),
                })
                .collect();
            rows.push(cols.join("|"));
        }
    }
    rows.sort();
    rows
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kfh-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn opts(limit: usize) -> Options {
    Options {
        since: (1, 0),
        limit: Some(limit),
        force: false,
    }
}

#[test]
fn adding_releases_matches_a_full_build() {
    let dir = scratch("incremental");
    let git = dir.join("linux.git");
    let header = "#define foo(x) bar(x, 1)\n";
    let bar = "int bar(int x, int y) { return x + y; }\nstatic int baz(void) { return 0; }\n";
    repo(
        &git,
        &[
            ("v1.0", &[("a.h", header), ("a.c", bar)]),
            (
                "v1.1",
                &[("a.h", header), ("a.c", "int bar(int x, long y) { return x + y; }\n")],
            ),
            // foo turns from a macro into a function. Its macro history is
            // kept only because of this release, after earlier ones were built.
            (
                "v1.2",
                &[("a.h", "int foo(int x);\n"), ("a.c", "int foo(int x) { return x; }\n")],
            ),
            (
                "v1.3",
                &[("a.h", "int foo(int x);\n"), ("a.c", "int foo(int x) { return x; }\n")],
            ),
        ],
    );

    let full = dir.join("full/functions.db");
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    build(&git, &full, &opts(4)).unwrap();

    let step = dir.join("step/functions.db");
    std::fs::create_dir_all(step.parent().unwrap()).unwrap();
    for limit in 1..=4 {
        build(&git, &step, &opts(limit)).unwrap();
    }

    let rows = dump(&full);
    assert!(
        rows.iter().any(|r| r.starts_with("span|foo|a.h|Macro|")),
        "the full build keeps foo's macro history: {rows:#?}"
    );
    assert_eq!(rows, dump(&step));
    assert_eq!(
        dump(&full.with_extension("state.db")),
        dump(&step.with_extension("state.db"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn force_starts_from_scratch() {
    let dir = scratch("force");
    let git = dir.join("linux.git");
    let v = "int bar(int x) { return x; }\n";
    repo(&git, &[("v1.0", &[("a.c", v)]), ("v1.1", &[("a.c", v)])]);
    let db = dir.join("functions.db");
    build(&git, &db, &opts(2)).unwrap();
    let before = dump(&db);
    build(&git, &db, &Options { force: true, ..opts(2) }).unwrap();
    assert_eq!(before, dump(&db));
    assert!(db.with_extension("state.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
