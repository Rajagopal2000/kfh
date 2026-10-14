//! Checks a freshly built database before it replaces the one being served.

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;

/// Functions every build since v4.0 defines. Their absence means extraction broke.
const SENTINELS: &[&str] = &["vfs_read", "tcp_sendmsg", "schedule", "kmem_cache_free"];

/// Largest drop in functions allowed for a release present in both databases.
const MAX_DROP: f64 = 0.01;

/// Fails if `candidate` looks worse than `current`, the database it would replace.
pub fn check_replacement(candidate: &Path, current: Option<&Path>) -> Result<()> {
    let new = open(candidate)?;
    let new_tags = tags(&new)?;
    let Some(latest) = new_tags.last() else {
        bail!("the new database has no releases");
    };
    for name in SENTINELS {
        ensure!(
            function_count(&new, latest, Some(name))? > 0,
            "`{name}` is not defined in {latest} of the new database"
        );
    }
    let Some(current) = current.filter(|p| p.exists()) else {
        return Ok(());
    };
    let old = open(current)?;
    let old_tags = tags(&old)?;
    ensure!(
        new_tags.starts_with(&old_tags),
        "the new database's releases ({}..{}) do not extend the current ones ({}..{})",
        new_tags.first().map_or("", String::as_str),
        latest,
        old_tags.first().map_or("", String::as_str),
        old_tags.last().map_or("", String::as_str)
    );
    if let Some(tag) = old_tags.last() {
        let before = function_count(&old, tag, None)?;
        let after = function_count(&new, tag, None)?;
        ensure!(
            after as f64 >= before as f64 * (1.0 - MAX_DROP),
            "{tag} has {after} functions in the new database, down from {before}"
        );
    }
    Ok(())
}

fn open(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {}", path.display()))
}

fn tags(db: &Connection) -> Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT tag FROM versions ORDER BY ord")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Functions (not macros) defined in release `tag`, optionally only those named `name`.
fn function_count(db: &Connection, tag: &str, name: Option<&str>) -> Result<i64> {
    Ok(db.query_row(
        "SELECT COUNT(DISTINCT d.name) FROM defs d
         JOIN spans s ON s.def_id = d.id
         JOIN signatures g ON g.id = s.sig_id
         JOIN versions v ON v.tag = ?1
         WHERE s.first_ord <= v.ord AND s.last_ord >= v.ord AND g.kind != 'Macro'
           AND (?2 IS NULL OR d.name = ?2)",
        params![tag, name],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::SCHEMA;

    /// A database with `tags`, each defining the sentinels plus `extra` functions.
    fn db(dir: &Path, file: &str, tags: &[&str], extra: usize) -> std::path::PathBuf {
        let path = dir.join(file);
        let _ = std::fs::remove_file(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        for (ord, tag) in tags.iter().enumerate() {
            db.execute("INSERT INTO versions VALUES (?1, ?2, 0)", params![ord as i64, tag])
                .unwrap();
        }
        db.execute(
            "INSERT INTO signatures (id, kind, return_type, params, is_static, is_inline, attributes) VALUES (1, 'Regular', 'int', '[]', 0, 0, '[]')",
            [],
        )
        .unwrap();
        let names: Vec<String> = SENTINELS
            .iter()
            .map(|s| s.to_string())
            .chain((0..extra).map(|i| format!("f{i}")))
            .collect();
        for (id, name) in names.iter().enumerate() {
            db.execute("INSERT INTO defs VALUES (?1, ?2, 'x.c')", params![id as i64, name])
                .unwrap();
            db.execute(
                "INSERT INTO spans (def_id, sig_id, first_ord, last_ord, lines) VALUES (?1, 1, 0, ?2, '1')",
                params![id as i64, tags.len() as i64 - 1],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn replacement_rules() {
        let dir = std::env::temp_dir().join(format!("kfh-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let current = db(&dir, "current.db", &["v7.1"], 1000);

        let grown = db(&dir, "grown.db", &["v7.1", "v7.2"], 1000);
        check_replacement(&grown, Some(&current)).unwrap();
        check_replacement(&grown, None).unwrap();

        let shrunk = db(&dir, "shrunk.db", &["v7.1", "v7.2"], 900);
        assert!(
            check_replacement(&shrunk, Some(&current))
                .unwrap_err()
                .to_string()
                .contains("down from")
        );

        let partial = db(&dir, "partial.db", &["v7.2"], 1000);
        assert!(
            check_replacement(&partial, Some(&current))
                .unwrap_err()
                .to_string()
                .contains("do not extend")
        );

        let empty = dir.join("empty.db");
        let _ = std::fs::remove_file(&empty);
        Connection::open(&empty).unwrap().execute_batch(SCHEMA).unwrap();
        assert!(check_replacement(&empty, None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
