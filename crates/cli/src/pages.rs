//! Server-rendered HTML. Everything a search engine indexes is rendered here;
//! `web/app.js` only adds search-as-you-type and tooltips.
//!
//! maud escapes every interpolated value, so names, paths and macro bodies
//! taken from kernel source cannot inject markup.

use extract::Param;
use history::query::{self, Change, DefHistory, Signature, Span, Version};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use std::collections::HashMap;

const ERA_COLORS: usize = 6;
/// Files shown before the rest are folded away.
const DEFS_SHOWN: usize = 6;
const EXAMPLES: &[&str] = &[
    "tcp_sendmsg",
    "do_sys_openat2",
    "vfs_read",
    "tcp_v4_connect",
    "__x64_sys_openat",
    "kmem_cache_alloc",
    "__alloc_pages",
    "wake_up_new_task",
];

/// What every page needs to know about the site.
pub struct Site<'a> {
    /// Absolute origin for canonical URLs, e.g. `https://kfh.rajagopaliyer.com`.
    pub origin: &'a str,
    /// Asset URLs, with a content hash so they can be cached for long.
    pub css: &'a str,
    pub js: &'a str,
}

/// Page metadata for the `<head>`.
struct Head<'a> {
    title: String,
    description: String,
    /// Path of the canonical URL; `None` for pages that must not be indexed.
    canonical: Option<String>,
    json_ld: Option<serde_json::Value>,
    search_value: &'a str,
}

fn layout(site: &Site, head: Head, body: Markup) -> Markup {
    let canonical = head.canonical.as_ref().map(|p| format!("{}{p}", site.origin));
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (head.title) }
                meta name="description" content=(head.description);
                @if let Some(url) = &canonical {
                    link rel="canonical" href=(url);
                    meta property="og:url" content=(url);
                } @else {
                    meta name="robots" content="noindex";
                }
                meta property="og:type" content="website";
                meta property="og:site_name" content="kfh";
                meta property="og:title" content=(head.title);
                meta property="og:description" content=(head.description);
                meta name="twitter:card" content="summary";
                link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Crect x='1' y='5' width='4' height='6' rx='1' fill='%233e6a9e'/%3E%3Crect x='6' y='5' width='4' height='6' rx='1' fill='%238a5aa8'/%3E%3Crect x='11' y='5' width='4' height='6' rx='1' fill='%232f8a78'/%3E%3C/svg%3E";
                link rel="preload" href="/fonts/ibm-plex-sans.woff2" as="font" type="font/woff2" crossorigin;
                link rel="preload" href="/fonts/ibm-plex-mono-400.woff2" as="font" type="font/woff2" crossorigin;
                link rel="stylesheet" href=(site.css);
                @if let Some(ld) = &head.json_ld {
                    // A data block, not a script: CSP does not apply and nothing executes.
                    script type="application/ld+json" { (PreEscaped(ld.to_string().replace("</", "<\\/"))) }
                }
                script src=(site.js) defer {}
            }
            body {
                header.bar {
                    a.mark href="/" { "kfh" }
                    form.search role="search" action="/search" method="get" {
                        label.visually-hidden for="q" { "Function name" }
                        input #q name="q" type="search" autocomplete="off" spellcheck="false"
                            value=(head.search_value)
                            placeholder="Search a kernel function, e.g. tcp_sendmsg"
                            role="combobox" aria-expanded="false" aria-controls="suggestions" aria-autocomplete="list";
                        ul #suggestions role="listbox" hidden {}
                    }
                }
                main #main { (body) }
                div #tip role="tooltip" hidden {}
            }
        }
    }
}

fn function_url(name: &str) -> String {
    format!("/f/{}", encode_path_segment(name))
}

fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn elixir(tag: &str, file: &str, line: u32) -> String {
    let path: Vec<String> = file.split('/').map(encode_path_segment).collect();
    format!(
        "https://elixir.bootlin.com/linux/{}/source/{}#L{line}",
        encode_path_segment(tag),
        path.join("/")
    )
}

/// `2024-03-10` from seconds since the Unix epoch (proleptic Gregorian).
pub fn iso_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn range_text(versions: &[Version]) -> String {
    match (versions.first(), versions.last()) {
        (Some(a), Some(b)) => format!("{} to {}", a.tag, b.tag),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------- landing

pub fn landing(site: &Site, versions: &[Version], names: usize) -> Markup {
    let range = range_text(versions);
    let description = format!(
        "Signature history of {names} Linux kernel functions across every mainline release from {range}: \
         parameter, return type and inlining changes, functions that became macros, and what to kprobe instead."
    );
    let json_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "WebSite",
        "name": "kfh: kernel function history",
        "url": site.origin,
        "description": description,
        "potentialAction": {
            "@type": "SearchAction",
            "target": format!("{}/search?q={{query}}", site.origin),
            "query-input": "required name=query",
        },
    });
    let body = html! {
        section.intro {
            h1 { "How has this kernel function changed?" }
            p {
                "Search a function to see its signature in every mainline Linux release from " (range)
                ": when parameters were added, removed or retyped, and when it moved, became inline, "
                "turned into a macro or disappeared. Each release links to the source on Elixir."
            }
            ul.examples {
                @for n in EXAMPLES {
                    li { a href=(function_url(n)) { (n) } }
                }
            }
        }
        section.about {
            h2 { "Why it exists" }
            p {
                "A kprobe attaches to a kernel function by name, so it breaks when that function is renamed, "
                "changes its parameters, gets inlined, or becomes a macro. kfh records " (format_count(names))
                " function names from the kernel source of every release, including names that macros "
                "build, such as the " code { "__x64_sys_*" } " syscall wrappers, and shows exactly which release changed what."
            }
            p {
                "The code is open source at "
                a href="https://github.com/Rajagopal2000/kfh" { "github.com/Rajagopal2000/kfh" } "."
            }
        }
    };
    layout(
        site,
        Head {
            title: "kfh: Linux kernel function signature history".into(),
            description,
            canonical: Some("/".into()),
            json_ld: Some(json_ld),
            search_value: "",
        },
        body,
    )
}

fn format_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------- not found

pub fn not_found(site: &Site, name: &str, versions: &[Version], suggestions: &[String]) -> Markup {
    let range = range_text(versions);
    let body = html! {
        h1.fn-name { (name) }
        @if suggestions.is_empty() {
            p.summary { "No function with this name is defined in " (range) ". Check the spelling." }
        } @else {
            p.summary { "No function with this name is defined in " (range) ". Did you mean one of these?" }
            ul.examples {
                @for n in suggestions {
                    li { a href=(function_url(n)) { (n) } }
                }
            }
        }
    };
    layout(
        site,
        Head {
            title: format!("{name}: not found | kfh"),
            description: format!("No Linux kernel function named {name} in {range}."),
            canonical: None,
            json_ld: None,
            search_value: name,
        },
        body,
    )
}

// ---------------------------------------------------------------- function

/// A span plus what the page computes about it.
struct Era<'a> {
    span: &'a Span,
    color: usize,
}

pub fn function(site: &Site, name: &str, versions: &[Version], mut defs: Vec<DefHistory>) -> Markup {
    let latest = versions.len() as i64 - 1;
    let last_ord = |d: &DefHistory| d.spans.iter().map(|s| s.last_ord).max().unwrap_or(-1);
    let presence = |d: &DefHistory| d.spans.iter().map(|s| s.last_ord - s.first_ord + 1).sum::<i64>();
    defs.sort_by(|a, b| {
        last_ord(b)
            .cmp(&last_ord(a))
            .then(presence(b).cmp(&presence(a)))
            .then(a.file.cmp(&b.file))
    });

    // One colour per distinct signature across the page, in order of first
    // appearance, so a signature keeps its colour when the function moves files.
    let mut all: Vec<&Span> = defs.iter().flat_map(|d| d.spans.iter()).collect();
    all.sort_by_key(|s| s.first_ord);
    let mut colors: Vec<Signature> = Vec::new();
    let mut era_color: HashMap<*const Span, usize> = HashMap::new();
    for s in &all {
        let seen = as_probed(&s.signature);
        let index = match colors.iter().position(|c| *c == seen) {
            Some(i) => i,
            None => {
                colors.push(seen);
                colors.len() - 1
            }
        };
        era_color.insert(*s as *const Span, index % ERA_COLORS);
    }

    let summary = summary(&defs, versions, latest);
    let description = description(name, &defs, versions, &summary);
    let date_modified = all
        .iter()
        .map(|s| s.first_ord)
        .max()
        .and_then(|o| versions.get(o as usize))
        .map(|v| iso_date(v.commit_time));
    let title = format!("{name}: Linux kernel function signature history | kfh");
    let json_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "TechArticle",
        "headline": format!("{name}: signature history across Linux {}", range_text(versions)),
        "description": description,
        "url": format!("{}{}", site.origin, function_url(name)),
        "dateModified": date_modified,
        "about": {
            "@type": "SoftwareSourceCode",
            "name": name,
            "programmingLanguage": "C",
            "codeRepository": "https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git",
        },
        "isPartOf": { "@type": "WebSite", "name": "kfh", "url": site.origin },
    });

    let render_def = |def: &DefHistory| -> Markup {
        let eras: Vec<Era> = def
            .spans
            .iter()
            .map(|s| Era {
                span: s,
                color: era_color[&(s as *const Span)],
            })
            .collect();
        def_section(name, def, &eras, versions, &defs)
    };
    let body = html! {
        h1.fn-name { (name) }
        p.summary { (summary) }
        @for def in defs.iter().take(DEFS_SHOWN) {
            (render_def(def))
        }
        @if defs.len() > DEFS_SHOWN {
            details.more-defs {
                summary.more {
                    @let rest = defs.len() - DEFS_SHOWN;
                    "Show " (rest) " more " (if rest == 1 { "file" } else { "files" })
                }
                @for def in defs.iter().skip(DEFS_SHOWN) {
                    (render_def(def))
                }
            }
        }
    };
    layout(
        site,
        Head {
            title,
            description,
            canonical: Some(function_url(name)),
            json_ld: Some(json_ld),
            search_value: name,
        },
        body,
    )
}

fn summary(defs: &[DefHistory], versions: &[Version], latest: i64) -> String {
    let tag = |o: i64| versions.get(o as usize).map_or("", |v| v.tag.as_str());
    let last_ord = |d: &DefHistory| d.spans.iter().map(|s| s.last_ord).max().unwrap_or(-1);
    let live = defs.iter().filter(|d| last_ord(d) == latest).count();
    let live_functions = defs
        .iter()
        .filter(|d| d.spans.iter().any(|s| s.last_ord == latest && !s.signature.is_macro()))
        .count();
    let spans = || {
        defs.iter()
            .flat_map(|d| d.spans.iter())
            .filter(|s| !s.changes.is_empty())
    };
    let changes = spans().filter(|s| s.changes.iter().any(Change::affects_probes)).count();
    let other = spans().count() - changes;
    let files = |n: usize| {
        if n == 1 {
            "1 file".to_string()
        } else {
            format!("{n} files")
        }
    };
    let mut s = format!("Defined in {}. ", files(defs.len()));
    if live == 0 {
        let last = defs.iter().map(last_ord).max().unwrap_or(0);
        s += &format!("Not present in {}; last seen in {}. ", tag(latest), tag(last));
    } else if live_functions == 0 {
        let last_fn = defs
            .iter()
            .flat_map(|d| d.spans.iter())
            .filter(|s| !s.signature.is_macro())
            .map(|s| s.last_ord)
            .max();
        s += &format!("In {} it is a macro, not a function", tag(latest));
        s += &match last_fn {
            Some(o) => format!("; it was last a function in {}. ", tag(o)),
            None => ". ".into(),
        };
    } else if live < defs.len() {
        s += &format!("Present in {} in {}. ", tag(latest), files(live));
    } else {
        s += &format!("Present in {}. ", tag(latest));
    }
    let first = defs
        .iter()
        .flat_map(|d| d.spans.iter())
        .map(|s| s.first_ord)
        .min()
        .unwrap_or(0);
    let since = if first == 0 {
        tag(0).to_string()
    } else {
        format!("it first appeared in {}", tag(first))
    };
    s += &match changes {
        0 => format!("No signature changes since {since}."),
        1 => format!("The signature changed once since {since}."),
        n => format!("The signature changed {n} times since {since}."),
    };
    s += &match other {
        0 => String::new(),
        1 => " One other change does not affect probes on x86-64 or arm64.".into(),
        n => format!(" {n} other changes do not affect probes on x86-64 or arm64."),
    };
    s
}

/// The signature as a probe sees it.
fn as_probed(sig: &Signature) -> Signature {
    let mut sig = sig.clone();
    sig.attributes.retain(|a| !query::PROBE_NEUTRAL.contains(&a.as_str()));
    sig.origin = None;
    sig
}

/// The meta description: the summary plus the newest signature, as plain text.
fn description(name: &str, defs: &[DefHistory], versions: &[Version], summary: &str) -> String {
    let newest = defs
        .iter()
        .flat_map(|d| d.spans.iter())
        .max_by_key(|s| (s.last_ord, s.first_ord))
        .map(|s| s.signature.render(name));
    let mut d = format!(
        "History of {name} in the Linux kernel, {}. {summary}",
        range_text(versions)
    );
    if let Some(sig) = newest {
        d += &format!(" Latest: {sig}");
    }
    if d.chars().count() > 300 {
        d = d.chars().take(297).collect::<String>() + "...";
    }
    d
}

fn def_section(name: &str, def: &DefHistory, eras: &[Era], versions: &[Version], defs: &[DefHistory]) -> Markup {
    let last = eras.iter().max_by_key(|e| e.span.last_ord).map(|e| e.span);
    let mut ordered: Vec<&Era> = eras.iter().collect();
    ordered.sort_by(|a, b| {
        b.span
            .last_ord
            .cmp(&a.span.last_ord)
            .then(b.span.first_ord.cmp(&a.span.first_ord))
    });
    html! {
        section.def {
            h2.def-file {
                @if let Some(s) = last {
                    a href=(elixir(&s.last, &def.file, s.lines.last().copied().unwrap_or(1))) rel="noopener" target="_blank" { (def.file) }
                } @else {
                    (def.file)
                }
            }
            (timeline(eras, versions))
            ol.eras {
                @for era in ordered {
                    (era_item(name, def, era, versions, defs))
                }
            }
        }
    }
}

fn timeline(eras: &[Era], versions: &[Version]) -> Markup {
    let n = versions.len();
    let mut prev: Option<usize> = None;
    let cells: Vec<Markup> = (0..n as i64)
        .map(|ord| {
            let tag = &versions[ord as usize].tag;
            // With overlapping #ifdef variants, show the one that started last.
            let era = eras
                .iter()
                .filter(|e| e.span.first_ord <= ord && ord <= e.span.last_ord)
                .max_by_key(|e| e.span.first_ord);
            let Some(era) = era else {
                prev = None;
                return html! { span.cell data-tip=(format!("{tag}: not defined in this file")) {} };
            };
            let line = era
                .span
                .lines
                .get((ord - era.span.first_ord) as usize)
                .copied()
                .unwrap_or(1);
            let boundary = prev.is_some_and(|p| p != era.color);
            prev = Some(era.color);
            let macro_ = era.span.signature.is_macro();
            let tip = format!("{tag}, line {line}{}", if macro_ { ", macro" } else { "" });
            let class = format!(
                "cell present{}{}",
                if boundary { " boundary" } else { "" },
                if macro_ { " macro" } else { "" }
            );
            // Visual only: cells are too small to tap, and each version's source is
            // linked from the list below.
            html! {
                span class=(class) style=(format!("--era: var(--era-{})", era.color)) data-tip=(tip) {}
            }
        })
        .collect();
    let majors: Vec<(usize, &str)> = versions
        .iter()
        .enumerate()
        .filter(|(i, v)| *i == 0 || major(&v.tag) != major(&versions[i - 1].tag))
        .map(|(i, v)| (i, v.tag.as_str()))
        .collect();
    html! {
        div.timeline {
            div.track style=(format!("--n: {n}")) aria-hidden="true" {
                @for c in cells { (c) }
            }
            div.ruler aria-hidden="true" {
                @for (i, tag) in majors {
                    // Labels near the right edge hang left of their tick.
                    span class=(if i * 10 > n * 9 { "end" } else { "" }) style=(format!("left: {}%", 100.0 * i as f64 / n as f64)) { (tag) }
                }
            }
        }
    }
}

fn major(tag: &str) -> &str {
    tag.split('.').next().unwrap_or(tag)
}

fn era_item(name: &str, def: &DefHistory, era: &Era, versions: &[Version], defs: &[DefHistory]) -> Markup {
    let span = era.span;
    let sig = &span.signature;
    let macro_ = sig.is_macro();
    let count = span.last_ord - span.first_ord + 1;
    let range = if span.first == span.last {
        span.first.clone()
    } else {
        format!("{} to {}", span.first, span.last)
    };
    html! {
        li class=(if macro_ { "era macro" } else { "era" }) style=(format!("--era: var(--era-{})", era.color)) {
            span.swatch aria-hidden="true" {}
            div.range { (range) small { (count) (if count == 1 { " release" } else { " releases" }) } }
            div {
                p.sig { (signature_html(name, span)) }
                @if let Some(by) = &sig.generated_by {
                    p.generated { "Generated by " code { (by) } }
                    @if let Some(origin) = span.origin.as_ref().or(sig.origin.as_ref()) {
                        p.generated { "Macro definitions used: " (origin_html(origin, &span.first)) }
                    }
                }
                (history(def, span))
                (gone(def, span, versions, defs))
                (tags(name, sig))
                @if macro_ { (targets(span)) }
                div.links {
                    "Source: "
                    a href=(elixir(&span.first, &def.file, span.lines.first().copied().unwrap_or(1))) rel="noopener" target="_blank" { (span.first) }
                    @if span.first != span.last {
                        ", "
                        a href=(elixir(&span.last, &def.file, span.lines.last().copied().unwrap_or(1))) rel="noopener" target="_blank" { (span.last) }
                    }
                }
            }
        }
    }
}

fn param_decl(p: &Param) -> String {
    match (&p.name, p.ty.as_str()) {
        (None, ty) => ty.to_string(),
        (Some(n), "") => n.clone(),
        (Some(n), ty) if ty.ends_with('*') => format!("{ty}{n}"),
        (Some(n), ty) => format!("{ty} {n}"),
    }
}

fn signature_html(name: &str, span: &Span) -> Markup {
    let sig = &span.signature;
    if sig.is_macro() {
        // Link the functions the macro calls, since those are what can be probed.
        let callees: Vec<&String> = span
            .targets
            .iter()
            .flat_map(|t| t.primary.iter().chain(&t.also))
            .collect();
        let body = sig.body.as_deref().unwrap_or("");
        let params: Vec<String> = sig.params.iter().map(query::macro_param).collect();
        return html! {
            "#define " (name) "(" (params.join(", ")) ") "
            @for (word, is_ident) in split_identifiers(body) {
                @if is_ident && callees.iter().any(|c| *c == word) {
                    a href=(function_url(word)) { (word) }
                } @else {
                    (word)
                }
            }
        };
    }
    let added: Vec<usize> = span
        .changes
        .iter()
        .filter_map(|c| match c {
            Change::ParamAdded { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let changed: Vec<&str> = span
        .changes
        .iter()
        .filter_map(|c| match c {
            Change::ParamTypeChanged { name, .. } => Some(name.as_str()),
            Change::ParamRenamed { to, .. } => Some(to.as_str()),
            _ => None,
        })
        .collect();
    let ret_changed = span.changes.iter().any(|c| matches!(c, Change::ReturnType { .. }));
    let ret = if sig.return_type.ends_with('*') {
        sig.return_type.clone()
    } else {
        format!("{} ", sig.return_type)
    };
    html! {
        @if ret_changed { span.p-changed { (ret) } } @else { (ret) }
        (name) "("
        @if sig.params.is_empty() { "void" }
        @for (i, p) in sig.params.iter().enumerate() {
            @if i > 0 { ", " }
            @if added.contains(&i) {
                span.p-added { (param_decl(p)) }
            } @else if p.name.as_deref().is_some_and(|n| changed.contains(&n)) {
                span.p-changed { (param_decl(p)) }
            } @else {
                (param_decl(p))
            }
        }
        ")"
    }
}

/// Splits text into identifier and non-identifier runs.
fn split_identifiers(s: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_ident = false;
    for (i, c) in s.char_indices() {
        let ident_char = c.is_ascii_alphanumeric() || c == '_';
        let starts_ident = !in_ident && (c.is_ascii_alphabetic() || c == '_');
        if starts_ident || (in_ident && !ident_char) {
            if i > start {
                out.push((&s[start..i], in_ident));
            }
            start = i;
            in_ident = starts_ident;
        }
    }
    if start < s.len() {
        out.push((&s[start..], in_ident));
    }
    out
}

/// An origin lists macro definitions as `file:line`; link each to its source.
fn origin_html(origin: &str, version: &str) -> Markup {
    let mut parts: Vec<Markup> = Vec::new();
    let mut rest = origin;
    while !rest.is_empty() {
        // A location is a path ending in .h or .c followed by :digits.
        let found = rest.match_indices(':').find_map(|(colon, _)| {
            let digits: String = rest[colon + 1..].chars().take_while(char::is_ascii_digit).collect();
            let path_start = rest[..colon].rfind([' ', ',', '(']).map_or(0, |p| p + 1);
            let path = &rest[path_start..colon];
            (!digits.is_empty() && (path.ends_with(".h") || path.ends_with(".c")))
                .then_some((path_start, colon, digits))
        });
        let Some((path_start, colon, digits)) = found else {
            parts.push(html! { (rest) });
            break;
        };
        let file = &rest[path_start..colon];
        let line: u32 = digits.parse().unwrap_or(1);
        parts.push(html! {
            (&rest[..path_start])
            a href=(elixir(version, file, line)) rel="noopener" target="_blank" { code { (file) ":" (digits) } }
        });
        rest = &rest[colon + 1 + digits.len()..];
    }
    html! { @for p in parts { (p) } }
}

fn history(def: &DefHistory, span: &Span) -> Markup {
    if span.moved_from.is_none() && span.changes.is_empty() {
        let previous_adjacent = def.spans.iter().any(|s| s.last_ord + 1 == span.first_ord);
        return html! {
            p.first-seen {
                @if span.first_ord == 0 {
                    "Already present in " (span.first) ", the oldest release covered."
                } @else if previous_adjacent {
                    "Variant in this file from " (span.first) "."
                } @else {
                    "Appears in this file from " (span.first) "."
                }
            }
        };
    }
    let kind_change = span
        .changes
        .iter()
        .find(|c| matches!(c, Change::KindChanged { from, to } if from == query::MACRO || to == query::MACRO));
    html! {
        ul.changes {
            @if let (Some(from), Some(Change::KindChanged { to, .. })) = (&span.moved_from, kind_change) {
                @let (was, now) = if to == query::MACRO { ("function", "macro") } else { ("macro", "function") };
                li.changed { "In " (span.first) ", the " (was) " in " code { (from) } " was replaced by this " (now) }
            } @else if let Some(from) = &span.moved_from {
                li.changed { "In " (span.first) ", moved here from " code { (from) } }
            }
            @for c in &span.changes {
                @if !(span.moved_from.is_some() && Some(c) == kind_change) {
                    li class=(change_class(c)) { (describe(c, &span.first)) }
                }
            }
        }
    }
}

/// Says when nothing with this name follows in this file.
fn gone(def: &DefHistory, span: &Span, versions: &[Version], defs: &[DefHistory]) -> Markup {
    let latest = versions.len() as i64 - 1;
    let succeeded = def
        .spans
        .iter()
        .any(|s| !std::ptr::eq(s, span) && s.first_ord > span.first_ord && s.first_ord <= span.last_ord + 1);
    if span.last_ord >= latest || succeeded {
        return html! {};
    }
    let next = &versions[(span.last_ord + 1) as usize].tag;
    let moved_to = defs.iter().find(|d| {
        d.spans
            .iter()
            .any(|s| s.moved_from.as_deref() == Some(def.file.as_str()) && s.first_ord == span.last_ord + 1)
    });
    let successor = moved_to.and_then(|d| d.spans.iter().find(|s| s.first_ord == span.last_ord + 1));
    match (moved_to, successor) {
        (Some(d), Some(s)) if s.signature.is_macro() != span.signature.is_macro() => html! {
            p.gone {
                "In " (next) ", replaced by a " (if span.signature.is_macro() { "function" } else { "macro" })
                " in " code { (d.file) } "."
            }
        },
        (Some(d), _) => html! { p.first-seen { "In " (next) ", moved to " code { (d.file) } "." } },
        _ => html! { p.gone { "Not defined in this file from " (next) " on." } },
    }
}

fn tag_help(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "static" => {
            "Visible only inside its file. The compiler may inline it or emit it with a suffix such as .isra.0, so the plain name may not be probe-able."
        }
        "inline" => "Marked inline. There is usually no standalone symbol, so a kprobe on this name will likely fail.",
        "__always_inline" => "Always inlined. No standalone symbol exists to probe.",
        "__init" => "Freed after boot. It cannot be probed once the system is up.",
        "__exit" => "Only used on module unload, and discarded when built into the kernel.",
        "notrace" => "Excluded from ftrace, so fentry and the function tracer cannot attach.",
        "noinstr" => "Excluded from instrumentation, including kprobes.",
        "__weak" => "Weak symbol. An architecture may override it with its own definition.",
        "noinline" => "Never inlined, so a standalone symbol exists to probe.",
        "__used" => "Kept in the build even if nothing calls it.",
        "__kprobes" => "Placed in .kprobes.text. kprobes refuse to attach to functions there.",
        "__bpf_kfunc" => "Callable from BPF programs. Never inlined, so a standalone symbol exists.",
        "__init_or_module" => "Freed after boot in kernels built without module support.",
        "asmlinkage" => {
            "Marks a function called from assembly. Only 32‑bit x86 and ia64 (removed in v6.7) pass its arguments differently; elsewhere it does nothing."
        }
        "__must_check" => {
            "Callers get a compiler warning if they ignore the result. The compiled function is the same."
        }
        "__printf" => "Lets the compiler check callers' format strings. The compiled function is the same.",
        "__maybe_unused" | "__always_unused" => "Silences unused warnings. The compiled function is the same.",
        "__releases" | "__acquires" | "__must_hold" | "__cond_acquires" => {
            "Lock annotation for static checkers (sparse, Clang). The compiled function is the same."
        }
        "__sched" => "Placed in .sched.text, so reports of where a task sleeps skip it. Probes are unaffected.",
        "__no_kcsan" => "Not instrumented by KCSAN, the data race detector. Probes are unaffected.",
        "__no_sanitize_address" => "Not instrumented by KASAN. Probes are unaffected.",
        "syscall" => {
            "Defined with SYSCALL_DEFINE. On x86-64 since v4.17, probe the wrapper __x64_sys_<name> rather than sys_<name>."
        }
        "compat_syscall" => "Defined with COMPAT_SYSCALL_DEFINE for 32-bit callers on 64-bit kernels.",
        "bpf_helper" => "Defined with BPF_CALL_n. The helper wrapper takes u64 arguments.",
        "macro" => {
            "A preprocessor macro, not a function. No symbol with this name exists to probe; probe a function it expands to instead."
        }
        "generated" => {
            "Defined by a macro, so this name never appears literally in the source. The line links to the macro invocation."
        }
        _ => return None,
    })
}

const WARN_TAGS: &[&str] = &[
    "macro",
    "inline",
    "__always_inline",
    "__init",
    "__exit",
    "notrace",
    "noinstr",
    "__kprobes",
];

fn tags(name: &str, sig: &Signature) -> Markup {
    let mut tags: Vec<&str> = Vec::new();
    if sig.generated_by.is_some() {
        tags.push("generated");
    }
    match sig.kind.as_str() {
        "Syscall" => tags.push("syscall"),
        "CompatSyscall" => tags.push("compat_syscall"),
        "BpfCall" => tags.push("bpf_helper"),
        "Macro" => tags.push("macro"),
        _ => {}
    }
    if sig.is_static {
        tags.push("static");
    }
    if sig.is_inline && !sig.attributes.iter().any(|a| a == "__always_inline") {
        tags.push("inline");
    }
    tags.extend(sig.attributes.iter().map(String::as_str));
    if tags.is_empty() {
        return html! {};
    }
    let syscall = name
        .strip_prefix("compat_sys_")
        .or_else(|| name.strip_prefix("sys_"))
        .unwrap_or(name);
    html! {
        div.tags {
            @for t in tags {
                @let class = if WARN_TAGS.contains(&t) { "tag warn" } else { "tag" };
                @if let Some(help) = tag_help(t) {
                    span class=(class) tabindex="0" data-tip=(help.replace("<name>", syscall)) { (t) }
                } @else {
                    span class=(class) { (t) }
                }
            }
        }
    }
}

/// What a macro calls, per range of releases.
fn targets(span: &Span) -> Markup {
    let whole = span.targets.len() == 1
        && span.targets[0].first_ord <= span.first_ord
        && span.targets[0].last_ord >= span.last_ord;
    let links = |names: &[String]| {
        html! {
            @for (i, n) in names.iter().enumerate() {
                @if i > 0 { ", " }
                a href=(function_url(n)) { code { (n) } }
            }
        }
    };
    html! {
        @for t in &span.targets {
            p.probe {
                @if !whole {
                    span.when { (if t.first == t.last { t.first.clone() } else { format!("{} to {}", t.first, t.last) }) }
                }
                @if t.primary.is_empty() {
                    "No function receives this macro's arguments."
                } @else {
                    "Probe instead: " (links(&t.primary))
                }
            }
            @if !t.also.is_empty() {
                p.also { "Also calls " (links(&t.also)) }
            }
        }
    }
}

fn change_class(c: &Change) -> &'static str {
    if !c.affects_probes() {
        return "neutral";
    }
    match c {
        Change::ParamAdded { .. } | Change::AttributeAdded { .. } => "added",
        Change::ParamRemoved { .. } | Change::AttributeRemoved { .. } => "removed",
        _ => "changed",
    }
}

/// An annotation, with its explanation as a tooltip where there is one.
fn attribute(name: &str) -> Markup {
    html! {
        @if let Some(help) = tag_help(name) {
            code.explained tabindex="0" data-tip=(help) { (name) }
        } @else {
            code { (name) }
        }
    }
}

fn probe_note(c: &Change) -> &'static str {
    if c.affects_probes() {
        ""
    } else {
        ", which does not affect probes on x86-64 or arm64"
    }
}

fn describe(c: &Change, version: &str) -> Markup {
    html! {
        "In " (version) ", "
        @match c {
            Change::ParamAdded { index, param } => { "added " code { (param_decl(param)) } " as parameter " (index + 1) }
            Change::ParamRemoved { index, param } => { "removed " code { (param_decl(param)) } ", which was parameter " (index + 1) }
            Change::ParamTypeChanged { name, from, to } => { code { (name) } " changed type from " code { (from) } " to " code { (to) } }
            Change::ParamRenamed { index, from, to } => { "parameter " (index + 1) " was renamed from " code { (from) } " to " code { (to) } }
            Change::ParamsReordered => { "the parameters were reordered" }
            Change::ReturnType { from, to } => { "the return type changed from " code { (from) } " to " code { (to) } }
            Change::StaticChanged { to } => { (if *to { "it became static" } else { "it stopped being static" }) }
            Change::InlineChanged { to } => { (if *to { "it became inline" } else { "it stopped being inline" }) }
            Change::AttributeAdded { name } => { (attribute(name)) " was added" (probe_note(c)) }
            Change::AttributeRemoved { name } => { (attribute(name)) " was removed" (probe_note(c)) }
            Change::KindChanged { from, to } => {
                @if to == query::MACRO { "it became a macro" }
                @else if from == query::MACRO { "it became a function" }
                @else { "it changed from " (from) " to " (to) }
            }
            Change::MacroBodyChanged { from, .. } => { "the expansion changed from " code { (from) } }
            Change::GeneratedByChanged { from, to } => {
                @match (from, to) {
                    (_, Some(to)) => {
                        "generated by " code { (to) }
                        @if let Some(from) = from { " instead of " code { (from) } }
                    }
                    (Some(from), None) => { "no longer generated by " code { (from) } }
                    (None, None) => {}
                }
            }
            Change::OriginChanged { from, to } => {
                @match (from, to) {
                    (_, Some(to)) => { "now comes from " code { (to) } }
                    (Some(from), None) => { "no longer depends on " code { (from) } }
                    (None, None) => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------- crawling

pub fn robots(origin: &str) -> String {
    format!("User-agent: *\nDisallow: /api/\nDisallow: /search\nSitemap: {origin}/sitemap.xml\n")
}

/// URLs per sitemap file (the protocol allows up to 50,000).
pub const SITEMAP_PAGE: usize = 50_000;

pub fn sitemap_index(origin: &str, names: usize, lastmod: &str) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for i in 0..names.div_ceil(SITEMAP_PAGE) {
        s += &format!("<sitemap><loc>{origin}/sitemaps/{i}.xml</loc><lastmod>{lastmod}</lastmod></sitemap>\n");
    }
    s += "</sitemapindex>\n";
    s
}

pub fn sitemap(origin: &str, entries: &[(String, i64)], include_home: bool) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    if include_home {
        s += &format!("<url><loc>{origin}/</loc></url>\n");
    }
    for (name, time) in entries {
        // Percent-encoding leaves nothing XML treats specially.
        s += &format!(
            "<url><loc>{origin}{}</loc><lastmod>{}</lastmod></url>\n",
            function_url(name),
            iso_date(*time)
        );
    }
    s += "</urlset>\n";
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(951_782_400), "2000-02-29");
        assert_eq!(iso_date(1_760_400_000), "2025-10-14");
    }

    #[test]
    fn identifiers_split() {
        assert_eq!(
            split_identifiers("alloc_hooks(kmem_cache_alloc_noprof(__VA_ARGS__)) + 0x1f"),
            [
                ("alloc_hooks", true),
                ("(", false),
                ("kmem_cache_alloc_noprof", true),
                ("(", false),
                ("__VA_ARGS__", true),
                (")) + 0", false),
                ("x1f", true),
            ]
        );
    }

    #[test]
    fn origins_link_each_location() {
        let html = origin_html(
            "include/linux/syscalls.h:245 (architectures without their own definition)",
            "v7.2",
        )
        .into_string();
        assert!(html.contains("https://elixir.bootlin.com/linux/v7.2/source/include/linux/syscalls.h#L245"));
        assert!(html.contains("<code>include/linux/syscalls.h:245</code>"));
        assert!(html.ends_with(" (architectures without their own definition)"));
        let two = origin_html("all except a/b.h:1, c/d.c:2", "v1.0").into_string();
        assert_eq!(two.matches("<a ").count(), 2);
        assert!(two.starts_with("all except "));
    }

    #[test]
    fn urls_are_encoded() {
        assert_eq!(function_url("tcp_sendmsg"), "/f/tcp_sendmsg");
        assert_eq!(function_url("a<b>\"&"), "/f/a%3Cb%3E%22%26");
        assert_eq!(
            elixir("v7.2", "fs/read write.c", 3),
            "https://elixir.bootlin.com/linux/v7.2/source/fs/read%20write.c#L3"
        );
    }

    fn openat(attributes: &[&str], first_ord: i64, last_ord: i64, changes: Vec<Change>) -> Span {
        Span {
            first: format!("v{first_ord}"),
            last: format!("v{last_ord}"),
            first_ord,
            last_ord,
            signature: Signature {
                kind: "Regular".into(),
                return_type: "long".into(),
                params: vec![Param {
                    ty: "const struct pt_regs *".into(),
                    name: Some("regs".into()),
                }],
                is_static: false,
                is_inline: false,
                attributes: attributes.iter().map(|a| a.to_string()).collect(),
                body: None,
                generated_by: None,
                origin: None,
            },
            lines: vec![],
            changes,
            moved_from: None,
            origin: None,
            targets: vec![],
        }
    }

    fn versions(n: i64) -> Vec<Version> {
        (0..n)
            .map(|ord| Version {
                ord,
                tag: format!("v{ord}"),
                commit_time: 0,
            })
            .collect()
    }

    #[test]
    fn changes_a_probe_cannot_see_are_kept_apart() {
        let removed = |name: &str| Change::AttributeRemoved { name: name.into() };
        let def = |name: &str| DefHistory {
            name: "f".into(),
            file: "fs/open.c".into(),
            spans: vec![openat(&[name], 1, 2, vec![]), openat(&[], 3, 4, vec![removed(name)])],
        };

        let neutral = def("asmlinkage");
        let text = summary(std::slice::from_ref(&neutral), &versions(5), 4);
        assert!(
            text.contains("No signature changes since it first appeared in v1."),
            "{text}"
        );
        assert!(
            text.ends_with(" One other change does not affect probes on x86-64 or arm64."),
            "{text}"
        );
        assert_eq!(
            as_probed(&neutral.spans[0].signature),
            as_probed(&neutral.spans[1].signature)
        );
        assert_eq!(change_class(&removed("asmlinkage")), "neutral");

        let traced = def("notrace");
        let text = summary(std::slice::from_ref(&traced), &versions(5), 4);
        assert!(
            text.ends_with("The signature changed once since it first appeared in v1."),
            "{text}"
        );
        assert_ne!(
            as_probed(&traced.spans[0].signature),
            as_probed(&traced.spans[1].signature)
        );
        assert_eq!(change_class(&removed("notrace")), "removed");
    }

    #[test]
    fn sitemaps() {
        let idx = sitemap_index("https://x", 120_000, "2026-10-01");
        assert_eq!(idx.matches("<sitemap>").count(), 3);
        let one = sitemap("https://x", &[("vfs_read".into(), 0)], true);
        assert!(one.contains("<loc>https://x/f/vfs_read</loc><lastmod>1970-01-01</lastmod>"));
        assert!(one.contains("<loc>https://x/</loc>"));
    }
}
