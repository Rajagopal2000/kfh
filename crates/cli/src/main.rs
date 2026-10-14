use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use history::query::{self, Change};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

mod serve;

/// Kernel function history: how function signatures evolved across Linux releases.
#[derive(Parser)]
#[command(name = "kfh")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the database from a Linux git repository.
    Build {
        #[arg(long, default_value = "data/linux.git")]
        repo: PathBuf,
        #[arg(long, default_value = "data/functions.db")]
        db: PathBuf,
        /// Oldest release to include, e.g. 4.0.
        #[arg(long, default_value = "4.0", value_parser = parse_version)]
        since: (u32, u32),
        /// Only process this many releases.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show the signature history of a function.
    Show {
        name: String,
        #[arg(long, default_value = "data/functions.db")]
        db: PathBuf,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Serve the web UI.
    Serve {
        #[arg(long, default_value = "data/functions.db")]
        db: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7878")]
        addr: String,
    },
    /// Print the functions a single source file defines (for debugging the extractor).
    DumpSigs { file: PathBuf },
}

fn parse_version(s: &str) -> Result<(u32, u32), String> {
    let (a, b) = s
        .trim_start_matches('v')
        .split_once('.')
        .ok_or("expected MAJOR.MINOR")?;
    Ok((a.parse().map_err(|_| "bad major")?, b.parse().map_err(|_| "bad minor")?))
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Build { repo, db, since, limit } => {
            history::build::build(&repo, &db, &history::build::Options { since, limit })
        }
        Command::Show { name, db, json } => show(&name, &db, json),
        Command::Serve { db, addr } => serve::serve(db, &addr),
        Command::DumpSigs { file } => {
            let src = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            for f in extract::extract(&src) {
                let sig = query::Signature {
                    kind: format!("{:?}", f.kind),
                    return_type: f.return_type,
                    params: f.params,
                    is_static: f.is_static,
                    is_inline: f.is_inline,
                    attributes: f.attributes,
                    body: f.body,
                    generated_by: f.generated_by,
                    origin: f.origin,
                };
                println!("{:>6}  {}  {:?}", f.line, sig.render(&f.name), sig.attributes);
            }
            Ok(())
        }
    }
}

fn show(name: &str, db: &PathBuf, json: bool) -> Result<()> {
    let db = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} (run `kfh build` first)", db.display()))?;
    let defs = query::history(&db, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&defs)?);
        return Ok(());
    }
    if defs.is_empty() {
        let suggestions = query::search(&db, name, 10)?;
        anyhow::bail!("no function named `{name}`. Names starting with it: {suggestions:?}");
    }
    for def in &defs {
        println!("{}", def.file);
        for span in &def.spans {
            let range = if span.first == span.last {
                span.first.clone()
            } else {
                format!("{}..{}", span.first, span.last)
            };
            if let Some(from) = &span.moved_from {
                println!("    {:>14}  ~ moved here from {from}", "");
            }
            for c in &span.changes {
                println!("    {:>14}  ~ {}", "", describe(c));
            }
            let mut extras = Vec::new();
            if span.signature.is_static {
                extras.push("static".to_string());
            }
            if span.signature.is_inline {
                extras.push("inline".to_string());
            }
            extras.extend(span.signature.attributes.iter().cloned());
            let extras = if extras.is_empty() {
                String::new()
            } else {
                format!("  [{}]", extras.join(" "))
            };
            println!("    {range:>14}  {}{extras}", span.signature.render(&def.name));
            println!(
                "    {:>14}  https://elixir.bootlin.com/linux/{}/source/{}#L{}",
                "",
                span.last,
                def.file,
                span.lines.last().copied().unwrap_or(1)
            );
        }
        println!();
    }
    Ok(())
}

fn describe(c: &Change) -> String {
    let p = |p: &extract::Param| format!("`{}`", query::param_decl(p));
    match c {
        Change::ReturnType { from, to } => format!("return type `{from}` -> `{to}`"),
        Change::ParamAdded { index, param } => format!("param {} added at position {}", p(param), index + 1),
        Change::ParamRemoved { index, param } => format!("param {} removed from position {}", p(param), index + 1),
        Change::ParamTypeChanged { name, from, to } => format!("param `{name}` type `{from}` -> `{to}`"),
        Change::ParamRenamed { index, from, to } => format!("param {} renamed `{from}` -> `{to}`", index + 1),
        Change::ParamsReordered => "params reordered".into(),
        Change::StaticChanged { to } => (if *to { "became static" } else { "no longer static" }).into(),
        Change::InlineChanged { to } => (if *to { "became inline" } else { "no longer inline" }).into(),
        Change::AttributeAdded { name } => format!("gained `{name}`"),
        Change::AttributeRemoved { name } => format!("lost `{name}`"),
        Change::KindChanged { to, .. } if to == query::MACRO => "became a macro".into(),
        Change::KindChanged { from, .. } if from == query::MACRO => "became a function".into(),
        Change::KindChanged { from, to } => format!("kind `{from}` -> `{to}`"),
        Change::MacroBodyChanged { from, to } => format!("macro body `{from}` -> `{to}`"),
        Change::GeneratedByChanged { from, to } => format!(
            "generated by `{}` -> `{}`",
            from.as_deref().unwrap_or("-"),
            to.as_deref().unwrap_or("-")
        ),
        Change::OriginChanged { from, to } => format!(
            "via `{}` -> `{}`",
            from.as_deref().unwrap_or("-"),
            to.as_deref().unwrap_or("-")
        ),
    }
}
