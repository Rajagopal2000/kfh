//! Extracts function signatures from unpreprocessed Linux kernel C source.
//!
//! Kernel annotations (`__init`, `notrace`, `__always_inline`, ...) confuse a
//! plain C grammar, so before parsing they are overwritten with spaces. This
//! keeps byte offsets and line numbers stable, which lets type text be read
//! back from a less-blanked copy of the source.

pub mod cpp;
pub mod scope;

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;
use tree_sitter::{Node, Parser, Point};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Function {
    pub name: String,
    pub kind: Kind,
    pub return_type: String,
    pub params: Vec<Param>,
    pub is_static: bool,
    pub is_inline: bool,
    /// Kernel annotations seen on the definition, e.g. `__init`, `notrace`.
    pub attributes: Vec<String>,
    /// Replacement text of a function-like macro, whitespace-normalised.
    pub body: Option<String>,
    /// For a function produced by expanding a macro: the invocation, e.g.
    /// `DEFINE_SHOW_ATTRIBUTE(foo)`.
    pub generated_by: Option<String>,
    /// Files whose macro definitions were chosen where several existed.
    pub origin: Option<String>,
    /// 1-based line of the function name.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Param {
    pub ty: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Regular,
    /// `SYSCALL_DEFINEn(name, ...)`, recorded as `sys_<name>`.
    Syscall,
    /// `COMPAT_SYSCALL_DEFINEn(name, ...)`, recorded as `compat_sys_<name>`.
    CompatSyscall,
    /// `BPF_CALL_n(name, ...)`.
    BpfCall,
    /// Function-like macro, `#define name(args) body`. Params carry names only.
    Macro,
}

/// Annotations recorded as attributes and dropped from type text.
const ATTRIBUTES: &[&str] = &[
    "__init",
    "__exit",
    "__ref",
    "__meminit",
    "__meminitdata",
    "__init_memblock",
    "__init_or_module",
    "__cpuinit",
    "__cpuexit",
    "__devinit",
    "__devexit",
    "__head",
    "__sched",
    "__kprobes",
    "__always_inline",
    "noinline",
    "__noinline",
    "notrace",
    "asmlinkage",
    "__visible",
    "__weak",
    "__cold",
    "__hot",
    "__must_check",
    "__used",
    "__maybe_unused",
    "__always_unused",
    "__unused",
    "__noreturn",
    "__latent_entropy",
    "__attribute_const__",
    "__pure",
    "__malloc",
    "__flatten",
    "__nocfi",
    "noinstr",
    "__noinstr",
    "__cpuidle",
    "__irq_entry",
    "__softirqentry",
    "__bpf_kfunc",
    "__noclone",
    "__naked",
    "__lockfunc",
    "__no_kcsan",
    "__no_sanitize_address",
    "__no_kasan",
    "__no_profile",
    "__no_stack_protector",
    "__fastpath",
    "__init_refok",
    "__exception_irq_entry",
    "__nostackprotector",
    "__intentional_overflow",
    "__no_sanitize_or_inline",
    "__always_inline__",
    "__ksym",
    "__xipram",
    "__init_or_acpilib",
    "__tracepoint_string",
];

/// Annotations that take an argument list, e.g. `__printf(1, 2)`.
const ATTRIBUTES_WITH_ARGS: &[&str] = &[
    "__printf",
    "__scanf",
    "__acquires",
    "__releases",
    "__must_hold",
    "__cond_acquires",
    "__cond_releases",
    "__section",
    "__aligned",
    "__alloc_size",
    "__realloc_size",
    "__diag_ignore",
    "__nocapture",
    "__no_sanitize",
    "__assume_aligned",
    "__counted_by",
];

/// Sparse annotations: blanked for parsing but kept in type text, since
/// `char __user *` is more informative than `char *`.
const SPARSE: &[&str] = &[
    "__user",
    "__iomem",
    "__rcu",
    "__percpu",
    "__force",
    "__kernel",
    "__nocast",
    "__private",
    "__safe",
    "__bitwise",
    "__le_bitwise",
];

struct Prepared {
    /// Source with attributes and macro function headers blanked; type text is read from here.
    clean: Vec<u8>,
    /// `clean` with sparse annotations and function bodies also blanked; this is what gets parsed.
    parse: Vec<u8>,
    /// Attribute spans removed from `clean`.
    found: Vec<(Range<usize>, String)>,
    /// Functions recognised from macro headers such as `SYSCALL_DEFINEn`.
    functions: Vec<Function>,
    /// Kernel-style bodies: offset of the opening `{` and the end of the closing `}`.
    bodies: Vec<(usize, usize)>,
    /// The original source with only comments blanked: macro text comes from here.
    nocomment: Vec<u8>,
    /// `nocomment` with function bodies blanked: top-level invocations are found here.
    scan: Vec<u8>,
}

fn prepare(src: &[u8]) -> Prepared {
    let mut clean = src.to_vec();
    let mut found = Vec::new();
    let mut functions = Vec::new();

    blank_macro_headers(&mut clean, &mut functions);
    for (range, word) in annotation_spans(src) {
        if SPARSE.contains(&word.as_str()) {
            continue;
        }
        blank(&mut clean, range.clone());
        found.push((range, word));
    }
    let mut parse = clean.clone();
    for (range, word) in annotation_spans(&clean) {
        if SPARSE.contains(&word.as_str()) {
            blank(&mut parse, range);
        }
    }
    blank_comments(&mut parse);
    mask_typeof(&mut parse);
    let bodies = blank_bodies(&mut parse);
    let mut nocomment = src.to_vec();
    blank_comments(&mut nocomment);
    let mut scan = nocomment.clone();
    blank_bodies(&mut scan);
    Prepared {
        clean,
        parse,
        found,
        functions,
        bodies,
        nocomment,
        scan,
    }
}

/// Blanks comments. tree-sitter-c ends a multi-line `#define` early when a
/// `/* comment */` precedes the `\` continuation, then parses the rest of the
/// macro, and often the rest of the file, as code.
fn blank_comments(src: &mut [u8]) {
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'"' | b'\'' => {
                let quote = src[i];
                i += 1;
                while i < src.len() && src[i] != quote && src[i] != b'\n' {
                    i += if src[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(src.len(), |p| i + 2 + p + 2);
                blank(src, i..end);
                i = end;
            }
            b'/' if src.get(i + 1) == Some(&b'/') => {
                let mut end = i;
                while end < src.len() && !(src[end] == b'\n' && src[end - 1] != b'\\') {
                    end += 1;
                }
                blank(src, i..end);
                i = end;
            }
            _ => i += 1,
        }
    }
}

/// Replaces `typeof(...)` (any spelling) with a same-length run of `_`, a
/// plain identifier. tree-sitter-c cannot parse `typeof` inside an
/// expression, which the kernel's syscall macros produce in parameter types:
/// `__typeof(__builtin_choose_expr(__builtin_types_compatible_p(typeof(x), ...)))`.
/// Type text is read from the unmasked source, so it is unaffected.
fn mask_typeof(src: &mut [u8]) {
    let mut i = 0;
    while i < src.len() {
        if !(src[i].is_ascii_alphabetic() || src[i] == b'_')
            || (i > 0 && (src[i - 1].is_ascii_alphanumeric() || src[i - 1] == b'_'))
        {
            i += 1;
            continue;
        }
        let start = i;
        while i < src.len() && (src[i].is_ascii_alphanumeric() || src[i] == b'_') {
            i += 1;
        }
        if !matches!(&src[start..i], b"typeof" | b"__typeof" | b"__typeof__") {
            continue;
        }
        let open = src[i..].iter().position(|b| !b.is_ascii_whitespace()).map(|p| i + p);
        if let Some(close) = open.filter(|&o| src[o] == b'(').and_then(|o| matching_paren(src, o)) {
            for b in &mut src[start..=close] {
                if *b != b'\n' {
                    *b = b'_';
                }
            }
            i = close + 1;
        }
    }
}

/// The exact bytes handed to tree-sitter, for debugging the extractor.
pub fn parse_buffer(src: &[u8]) -> Vec<u8> {
    prepare(src).parse
}

/// Everything the history needs from one source file.
pub struct FileFacts {
    /// Function definitions, plus function-like macros as `Kind::Macro`.
    pub functions: Vec<Function>,
    /// Function-like macro definitions, for expanding macros.
    pub macros: Vec<MacroDefinition>,
    /// Names of object-like macros (`#define NAME value`).
    pub object_macros: Vec<String>,
    /// Top-level macro invocations, which may expand to function definitions.
    pub invocations: Vec<Invocation>,
    /// `#include` directives, in order.
    pub includes: Vec<Include>,
    /// `#undef NAME` directives as (name, line).
    pub undefs: Vec<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub path: String,
    /// `<path>` rather than `"path"`.
    pub system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDefinition {
    pub name: String,
    pub line: u32,
    pub params: Vec<String>,
    /// `__VA_ARGS__`, or the name of a GNU named variadic parameter.
    pub variadic: Option<String>,
    /// Replacement text with comments and line continuations removed.
    pub body: String,
    /// Enclosing `#if` branches.
    pub conditions: Vec<Cond>,
}

/// One enclosing branch of an `#if`/`#ifdef`/`#ifndef` block. Include
/// guards are not counted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cond {
    /// Block number within the file.
    pub block: u32,
    /// 0 for the first branch, then one per `#elif`/`#else`.
    pub branch: u32,
    /// Every condition of the block up to this branch tests only `CONFIG_*`
    /// options, so it does not depend on the architecture's own macros.
    pub config_only: bool,
}

/// A macro invocation at the start of a top-level line, such as
/// `DEFINE_SHOW_ATTRIBUTE(foo)` or the header `META_COLLECTOR(int_random)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub name: String,
    /// The invocation from the name to the closing parenthesis.
    pub text: String,
    pub line: u32,
    /// A function body follows, so the macro expands to a function header.
    pub followed_by_brace: bool,
    /// Enclosing `#if` branches.
    pub conditions: Vec<Cond>,
}

pub fn extract(src: &[u8]) -> Vec<Function> {
    analyze(src).functions
}

pub fn analyze(src: &[u8]) -> FileFacts {
    let Prepared {
        clean,
        parse,
        found,
        mut functions,
        bodies,
        nocomment,
        scan,
    } = prepare(src);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("tree-sitter-c grammar is compatible");

    // Pass 1: the whole file. Pass 2: kernel-style bodies whose header pass 1
    // missed get their header parsed in isolation, because one odd top-level
    // macro can derail error recovery for everything after it.
    let mut claimed = HashSet::new();
    if let Some(tree) = parser.parse(&parse, None) {
        collect(tree.root_node(), &clean, &parse, &found, &mut functions, &mut claimed);
    }
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            parse
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        )
        .collect();
    let point = |byte: usize| {
        let row = line_starts.partition_point(|&s| s <= byte) - 1;
        Point {
            row,
            column: byte - line_starts[row],
        }
    };
    for &(open, close_end) in &bodies {
        if claimed.contains(&open) {
            continue;
        }
        let start = header_start(&parse, open);
        let range = tree_sitter::Range {
            start_byte: start,
            end_byte: close_end,
            start_point: point(start),
            end_point: point(close_end),
        };
        if parser.set_included_ranges(&[range]).is_err() {
            continue;
        }
        if let Some(tree) = parser.parse(&parse, None) {
            collect(tree.root_node(), &clean, &parse, &found, &mut functions, &mut claimed);
        }
    }
    let conds = conditions(&nocomment);
    let (macros, object_macros) = macros(&nocomment, &parse, &conds);
    functions.extend(macros.iter().map(|m| {
        Function {
            name: m.name.clone(),
            kind: Kind::Macro,
            return_type: String::new(),
            params: m
                .params
                .iter()
                .map(|p| Param {
                    ty: String::new(),
                    name: Some(p.clone()),
                })
                .chain(m.variadic.iter().map(|v| Param {
                    ty: "...".into(),
                    name: (v != "__VA_ARGS__").then(|| v.clone()),
                }))
                .collect(),
            is_static: false,
            is_inline: false,
            attributes: Vec::new(),
            body: Some(m.body.split_whitespace().collect::<Vec<_>>().join(" ")),
            generated_by: None,
            origin: None,
            line: m.line,
        }
    }));
    functions.sort_by_key(|f| f.line);
    let (includes, undefs) = directives(&nocomment);
    FileFacts {
        functions,
        macros,
        object_macros,
        invocations: invocations(&nocomment, &scan, &conds),
        includes,
        undefs,
    }
}

/// The `#if` branches in effect on each line (0-based). The include guard
/// (a leading `#ifndef X` followed by `#define X`) is left out.
fn conditions(nocomment: &[u8]) -> Vec<Arc<[Cond]>> {
    struct Open {
        cond: Cond,
        guard: bool,
    }
    let mut stack: Vec<Open> = Vec::new();
    let mut out: Vec<Arc<[Cond]>> = Vec::new();
    let mut current: Arc<[Cond]> = Arc::from(Vec::new());
    let mut next_block = 0u32;
    let mut directives_seen = 0;
    let mut guard_candidate: Option<(u32, String)> = None;
    let mut continuing = false;
    for line in nocomment.split(|&b| b == b'\n') {
        out.push(current.clone());
        let text = String::from_utf8_lossy(line);
        let was_continuing = continuing;
        continuing = text.trim_end().ends_with('\\');
        if was_continuing {
            continue;
        }
        let Some(rest) = text.trim_start().strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim_start();
        let word: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        let arg = rest[word.len()..].trim();
        directives_seen += 1;
        let mut changed = true;
        match word.as_str() {
            "if" | "ifdef" | "ifndef" => {
                let config_only = match word.as_str() {
                    "if" => tests_only_config(arg),
                    _ => arg.starts_with("CONFIG_"),
                };
                stack.push(Open {
                    cond: Cond {
                        block: next_block,
                        branch: 0,
                        config_only,
                    },
                    guard: false,
                });
                if directives_seen == 1 && word == "ifndef" {
                    guard_candidate = Some((next_block, arg.to_string()));
                }
                next_block += 1;
            }
            "elif" | "elifdef" | "elifndef" | "else" => {
                if let Some(top) = stack.last_mut() {
                    top.cond.branch += 1;
                    top.cond.config_only &= match word.as_str() {
                        "else" => true,
                        "elif" => tests_only_config(arg),
                        _ => arg.starts_with("CONFIG_"),
                    };
                }
            }
            "endif" => {
                stack.pop();
            }
            "define" if directives_seen == 2 => {
                let name: String = arg
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if let Some((block, guard)) = &guard_candidate
                    && *guard == name
                    && let Some(top) = stack.iter_mut().find(|o| o.cond.block == *block)
                {
                    top.guard = true;
                }
            }
            _ => changed = false,
        }
        if changed {
            current = stack
                .iter()
                .filter(|o| !o.guard)
                .map(|o| o.cond.clone())
                .collect::<Vec<_>>()
                .into();
        }
    }
    out
}

/// Whether an `#if` expression tests nothing but `CONFIG_*` options.
fn tests_only_config(expr: &str) -> bool {
    const ALLOWED: &[&str] = &["defined", "IS_ENABLED", "IS_BUILTIN", "IS_MODULE", "IS_REACHABLE"];
    let mut word = String::new();
    for c in expr.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty()
            && !word.starts_with(|c: char| c.is_ascii_digit())
            && !word.starts_with("CONFIG_")
            && !ALLOWED.contains(&word.as_str())
        {
            return false;
        }
        word.clear();
    }
    true
}

/// `#include` and `#undef` directives. Computed includes (`#include MACRO`)
/// cannot be resolved without a configuration and are skipped.
fn directives(nocomment: &[u8]) -> (Vec<Include>, Vec<(String, u32)>) {
    let mut includes = Vec::new();
    let mut undefs = Vec::new();
    for (row, line) in nocomment.split(|&b| b == b'\n').enumerate() {
        let text = String::from_utf8_lossy(line);
        let Some(rest) = text.trim_start().strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim_start();
        if let Some(arg) = rest.strip_prefix("include") {
            let arg = arg.trim();
            let (system, close) = match arg.chars().next() {
                Some('<') => (true, '>'),
                Some('"') => (false, '"'),
                _ => continue,
            };
            if let Some(end) = arg[1..].find(close) {
                includes.push(Include {
                    path: arg[1..1 + end].to_string(),
                    system,
                });
            }
        } else if let Some(arg) = rest.strip_prefix("undef") {
            let name: String = arg
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if arg.starts_with([' ', '\t']) && !name.is_empty() {
                undefs.push((name, row as u32 + 1));
            }
        }
    }
    (includes, undefs)
}

/// Functions defined by already-expanded code, e.g. the expansion of
/// `DEFINE_SHOW_ATTRIBUTE(foo)`, and the names rejected because pasting
/// built them from values only the build knows (see `cpp::Token::tainted`).
pub fn functions_in_expansion(tokens: &[cpp::Token]) -> (Vec<Function>, Vec<String>) {
    let text = cpp::render_for_extraction(tokens);
    let tainted: HashSet<&str> = tokens.iter().filter(|t| t.tainted).map(|t| &*t.text).collect();
    let (rejected, kept): (Vec<Function>, Vec<Function>) = extract(text.as_bytes())
        .into_iter()
        .filter(|f| f.kind != Kind::Macro)
        .partition(|f| tainted.contains(f.name.as_str()));
    (kept, rejected.into_iter().map(|f| f.name).collect())
}

/// Words that may precede a macro invocation on its line.
const INVOCATION_PREFIXES: &[&str] = &["static", "const", "extern"];

/// Top-level macro invocations: an identifier and its parenthesised
/// arguments at the start of a line that is not inside a preprocessor
/// directive and does not continue a declaration from the line above.
fn invocations(nocomment: &[u8], scan: &[u8], conds: &[Arc<[Cond]>]) -> Vec<Invocation> {
    let mut out = Vec::new();
    let mut row = 0u32;
    let mut line_start = 0;
    let mut in_directive = false;
    let mut prev_line_open = false;
    while line_start < scan.len() {
        let line_end = scan[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(scan.len(), |i| line_start + i);
        let line = &scan[line_start..line_end];
        let trimmed = line.trim_ascii();
        let this_row = row;
        row += 1;
        let next_start = line_end + 1;

        if in_directive || trimmed.first() == Some(&b'#') {
            in_directive = trimmed.ends_with(b"\\");
            prev_line_open = false;
            line_start = next_start;
            continue;
        }
        let continues_declaration = prev_line_open;
        prev_line_open = !trimmed.is_empty() && !matches!(trimmed.last(), Some(b';' | b'}' | b'{' | b')'));
        if continues_declaration || !line.first().is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') {
            line_start = next_start;
            continue;
        }

        // Skip `static` and friends, then expect `NAME (`.
        let mut i = line_start;
        let name_range = loop {
            let start = i;
            while i < scan.len() && (scan[i].is_ascii_alphanumeric() || scan[i] == b'_') {
                i += 1;
            }
            let word = &scan[start..i];
            let mut j = i;
            while j < scan.len() && (scan[j] == b' ' || scan[j] == b'\t') {
                j += 1;
            }
            if word.is_empty() {
                break None;
            }
            if scan.get(j) == Some(&b'(') {
                break Some((start, i, j));
            }
            if !INVOCATION_PREFIXES.iter().any(|p| p.as_bytes() == word) || j == i {
                break None;
            }
            i = j;
        };
        let Some((name_start, name_end, open)) = name_range else {
            line_start = next_start;
            continue;
        };
        let Some(close) = matching_paren(scan, open) else {
            line_start = next_start;
            continue;
        };
        let after = scan[close + 1..]
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map(|p| close + 1 + p);
        let text = String::from_utf8_lossy(&nocomment[name_start..=close]).into_owned();
        out.push(Invocation {
            name: String::from_utf8_lossy(&scan[name_start..name_end]).into_owned(),
            text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            line: this_row + 1,
            followed_by_brace: after.is_some_and(|a| scan[a] == b'{'),
            conditions: conds.get(this_row as usize).map_or_else(Vec::new, |c| c.to_vec()),
        });
        // Continue after the invocation; it may span several lines.
        let spanned = scan[line_start..=close].iter().filter(|&&b| b == b'\n').count() as u32;
        row = this_row + 1 + spanned;
        let close_line_end = scan[close..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(scan.len(), |p| close + p);
        prev_line_open = false;
        line_start = close_line_end + 1;
    }
    out
}

/// Function-like macro definitions. Lines are located in `parse`, where
/// comments and function bodies are blank, so commented-out defines and
/// macros local to a function are skipped; text is taken from `nocomment`.
fn macros(nocomment: &[u8], parse: &[u8], conds: &[Arc<[Cond]>]) -> (Vec<MacroDefinition>, Vec<String>) {
    let mut out = Vec::new();
    let mut objects = Vec::new();
    let mut row = 0u32;
    let mut line_start = 0;
    while line_start < parse.len() {
        let line_end = parse[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(parse.len(), |i| line_start + i);
        let first_row = row;
        // Extend over `\` continuation lines.
        let mut end = line_end;
        while end < parse.len() && parse[line_start..end].trim_ascii_end().ends_with(b"\\") {
            end = parse[end + 1..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(parse.len(), |i| end + 1 + i);
            row += 1;
        }
        let visible = parse[line_start..line_end].trim_ascii_start().first() == Some(&b'#');
        if visible {
            match macro_from_line(&nocomment[line_start..end], first_row + 1) {
                Some(Ok(mut f)) => {
                    f.conditions = conds.get(first_row as usize).map_or_else(Vec::new, |c| c.to_vec());
                    out.push(f);
                }
                Some(Err(object)) => objects.push(object),
                None => {}
            }
        }
        row += 1;
        line_start = end + 1;
    }
    (out, objects)
}

/// A `#define` line: a function-like definition, or the name of an
/// object-like one.
fn macro_from_line(line: &[u8], line_no: u32) -> Option<Result<MacroDefinition, String>> {
    let text = String::from_utf8_lossy(line);
    let rest = text
        .trim_start()
        .strip_prefix('#')?
        .trim_start()
        .strip_prefix("define")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start();
    let name_len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let (name, after) = rest.split_at(name_len);
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    // Function-like only when the parenthesis follows the name directly.
    if !after.starts_with('(') {
        return Some(Err(name.to_string()));
    }
    let close = after.find(')')?;
    let mut params = Vec::new();
    let mut variadic = None;
    for p in after[1..close].split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match p.strip_suffix("...") {
            Some("") => variadic = Some("__VA_ARGS__".to_string()),
            Some(named) => variadic = Some(named.trim().to_string()),
            None => params.push(p.to_string()),
        }
    }
    let body = after[close + 1..].replace("\\\r\n", " ").replace("\\\n", " ");
    Some(Ok(MacroDefinition {
        name: name.into(),
        line: line_no,
        params,
        variadic,
        body: body.trim().to_string(),
        conditions: Vec::new(),
    }))
}

fn collect(
    root: Node,
    clean: &[u8],
    parse: &[u8],
    found: &[(Range<usize>, String)],
    out: &mut Vec<Function>,
    claimed: &mut HashSet<usize>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition" {
            let body_start = node.child_by_field_name("body").map(|b| b.start_byte());
            if let Some(f) = function_from_node(node, clean, parse, found)
                && body_start.is_none_or(|b| claimed.insert(b))
            {
                out.push(f);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

/// Start of the function header that precedes the body opening at `open`:
/// the contiguous non-blank lines above it, stopping at preprocessor lines
/// and lines that end a previous construct.
fn header_start(src: &[u8], open: usize) -> usize {
    let mut start = src[..open].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    for _ in 0..30 {
        if start == 0 {
            break;
        }
        let prev_start = src[..start - 1].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let line = String::from_utf8_lossy(&src[prev_start..start - 1]);
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.ends_with(';') || t.ends_with('}') || t.ends_with('{') {
            break;
        }
        start = prev_start;
    }
    start
}

fn function_from_node(def: Node, clean: &[u8], parse: &[u8], found: &[(Range<usize>, String)]) -> Option<Function> {
    def.child_by_field_name("type")?;
    let body = def.child_by_field_name("body")?;

    // Walk pointer declarators wrapping the function declarator (`struct page *foo(...)`).
    let mut decl = def.child_by_field_name("declarator")?;
    while decl.kind() != "function_declarator" {
        decl = match decl.kind() {
            "pointer_declarator" | "attributed_declarator" => decl.child_by_field_name("declarator")?,
            _ => return None,
        };
    }
    let name_node = decl.child_by_field_name("declarator")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = text(clean, name_node.byte_range());

    let mut is_static = false;
    let mut is_inline = false;
    let mut type_start = None;
    let mut excluded: Vec<Range<usize>> = Vec::new();
    let mut cursor = def.walk();
    for child in def.children(&mut cursor) {
        if child.start_byte() >= name_node.start_byte() {
            break;
        }
        match child.kind() {
            "storage_class_specifier" => {
                match text(clean, child.byte_range()).as_str() {
                    "static" => is_static = true,
                    "inline" | "__inline" | "__inline__" => is_inline = true,
                    _ => {}
                }
                excluded.push(child.byte_range());
            }
            "attribute_specifier" | "ms_declspec_modifier" | "ERROR" => excluded.push(child.byte_range()),
            _ => {
                type_start.get_or_insert(child.start_byte());
            }
        }
    }
    let type_start = type_start?;
    let return_type = normalize_type(&text_without(clean, type_start..name_node.start_byte(), &excluded));

    // Attributes may sit before the node start (`asmlinkage void foo`), so look
    // back over whitespace (blanked annotations included) to the previous token.
    let mut window_start = def.start_byte();
    while window_start > 0 && parse[window_start - 1].is_ascii_whitespace() {
        window_start -= 1;
    }
    let mut attributes: Vec<String> = found
        .iter()
        .filter(|(r, _)| r.start >= window_start && r.end <= body.start_byte())
        .map(|(_, w)| w.clone())
        .collect();
    attributes.dedup();
    if attributes.iter().any(|a| a == "__always_inline") {
        is_inline = true;
    }

    let params = params_from_list(decl.child_by_field_name("parameters")?, clean);

    Some(Function {
        name,
        kind: Kind::Regular,
        return_type,
        params,
        is_static,
        is_inline,
        attributes,
        body: None,
        generated_by: None,
        origin: None,
        line: name_node.start_position().row as u32 + 1,
    })
}

fn params_from_list(list: Node, clean: &[u8]) -> Vec<Param> {
    let mut params = Vec::new();
    let mut cursor = list.walk();
    for p in list.named_children(&mut cursor) {
        match p.kind() {
            "variadic_parameter" => params.push(Param {
                ty: "...".into(),
                name: None,
            }),
            "parameter_declaration" => {
                let name_node = p.child_by_field_name("declarator").and_then(declarator_name);
                let ty = match name_node {
                    Some(n) => text_without(clean, p.byte_range(), &[n.byte_range()]),
                    None => text(clean, p.byte_range()),
                };
                params.push(Param {
                    ty: normalize_type(&ty),
                    name: name_node.map(|n| text(clean, n.byte_range())),
                });
            }
            _ => {}
        }
    }
    if params.len() == 1 && params[0].ty == "void" && params[0].name.is_none() {
        params.clear();
    }
    params
}

/// Finds the identifier a (possibly nested) declarator declares.
fn declarator_name(mut node: Node) -> Option<Node> {
    loop {
        match node.kind() {
            "identifier" => return Some(node),
            "parenthesized_declarator" => node = node.named_child(0)?,
            _ => node = node.child_by_field_name("declarator")?,
        }
    }
}

/// Handles function headers written as macros (`SYSCALL_DEFINE3(read, ...)`),
/// which tree-sitter cannot parse. Recognised headers are recorded and blanked.
fn blank_macro_headers(src: &mut [u8], out: &mut Vec<Function>) {
    const MACROS: &[(&str, Kind)] = &[
        ("COMPAT_SYSCALL_DEFINE", Kind::CompatSyscall),
        ("SYSCALL_DEFINE", Kind::Syscall),
        ("BPF_CALL_", Kind::BpfCall),
    ];
    let mut line_start = 0;
    while line_start < src.len() {
        let line_end = src[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(src.len(), |i| line_start + i);
        let indent = src[line_start..line_end]
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
        let start = line_start + indent;
        for &(prefix, kind) in MACROS {
            if !src[start..].starts_with(prefix.as_bytes()) {
                continue;
            }
            let digit = start + prefix.len();
            if !src.get(digit).is_some_and(u8::is_ascii_digit) || src.get(digit + 1) != Some(&b'(') {
                continue;
            }
            let Some(close) = matching_paren(src, digit + 1) else {
                continue;
            };
            let after = src[close + 1..]
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map(|i| close + 1 + i);
            if after.map(|i| src[i]) != Some(b'{') {
                continue;
            }
            let args = split_args(&String::from_utf8_lossy(&src[digit + 2..close]));
            if let Some((name, rest)) = args.split_first() {
                let (name, return_type) = match kind {
                    Kind::Syscall => (format!("sys_{name}"), "long"),
                    Kind::CompatSyscall => (format!("compat_sys_{name}"), "long"),
                    _ => (name.clone(), "u64"),
                };
                let params = rest
                    .chunks(2)
                    .map(|c| Param {
                        ty: normalize_type(&c[0]),
                        name: c.get(1).cloned(),
                    })
                    .collect();
                out.push(Function {
                    name,
                    kind,
                    return_type: return_type.into(),
                    params,
                    is_static: false,
                    is_inline: false,
                    attributes: Vec::new(),
                    body: None,
                    generated_by: None,
                    origin: None,
                    line: src[..start].iter().filter(|&&b| b == b'\n').count() as u32 + 1,
                });
                blank(src, start..close + 1);
            }
            break;
        }
        line_start = line_end + 1;
    }
}

/// Blanks function bodies written in kernel style: `{` alone on a line and a
/// closing `}` in column 0. Only headers matter here, and bodies hold the
/// macro tricks (`#define` inside a function, statement macros without `;`)
/// that otherwise derail error recovery for the rest of the file.
fn blank_bodies(src: &mut [u8]) -> Vec<(usize, usize)> {
    let mut bodies = Vec::new();
    let mut open: Option<usize> = None;
    let mut line_start = 0;
    while line_start < src.len() {
        let line_end = src[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(src.len(), |i| line_start + i);
        let line = &src[line_start..line_end];
        match open {
            None if line.strip_suffix(b"\r").unwrap_or(line) == b"{" => open = Some(line_end),
            Some(body_start) if line.first() == Some(&b'}') => {
                blank(src, body_start..line_start);
                let brace = src[..body_start]
                    .iter()
                    .rposition(|&b| b == b'{')
                    .expect("body opens with {");
                bodies.push((brace, line_start + 1));
                open = None;
            }
            _ => {}
        }
        line_start = line_end + 1;
    }
    bodies
}

fn matching_paren(src: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in src.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                args.push(normalize_type(&cur));
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        args.push(normalize_type(&cur));
    }
    args
}

/// Yields the spans of known annotations, including the argument list of
/// annotations that take one.
fn annotation_spans(src: &[u8]) -> Vec<(Range<usize>, String)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if !(b.is_ascii_alphabetic() || b == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < src.len() && (src[i].is_ascii_alphanumeric() || src[i] == b'_') {
            i += 1;
        }
        if start > 0 && src[start - 1].is_ascii_digit() {
            continue;
        }
        let word = std::str::from_utf8(&src[start..i]).unwrap_or("");
        if ATTRIBUTES.contains(&word) || SPARSE.contains(&word) {
            spans.push((start..i, word.to_string()));
        } else if ATTRIBUTES_WITH_ARGS.contains(&word) {
            let open = src[i..].iter().position(|b| !b.is_ascii_whitespace()).map(|o| i + o);
            if let Some(open) = open.filter(|&o| src[o] == b'(')
                && let Some(close) = matching_paren(src, open)
            {
                spans.push((start..close + 1, word.to_string()));
                i = close + 1;
            }
        }
    }
    spans
}

fn blank(src: &mut [u8], range: Range<usize>) {
    for b in &mut src[range] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

fn text(src: &[u8], range: Range<usize>) -> String {
    String::from_utf8_lossy(&src[range]).into_owned()
}

fn text_without(src: &[u8], range: Range<usize>, excluded: &[Range<usize>]) -> String {
    let bytes: Vec<u8> = range
        .filter(|i| !excluded.iter().any(|r| r.contains(i)))
        .map(|i| src[i])
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Canonical spelling for type text so cosmetic edits do not register as
/// signature changes: single spaces, `char *`, `int (*)(void)`, `int[]`.
pub fn normalize_type(s: &str) -> String {
    let s = strip_comments(s);
    let mut out = String::with_capacity(s.len());
    for token in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(token);
    }
    // Remove spaces around punctuation, then put one back before a `*` that follows a word.
    let mut tight = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let prev = chars[..i].last().copied().unwrap_or(' ');
            let next = chars.get(i + 1).copied().unwrap_or(' ');
            if "*([,".contains(prev) || "*)[],".contains(next) || (prev == ')' && next == '(') {
                continue;
            }
        }
        tight.push(c);
    }
    let mut result = String::with_capacity(tight.len());
    let mut prev = ' ';
    for c in tight.chars() {
        if c == '*' && (prev.is_alphanumeric() || prev == '_' || prev == ')') {
            result.push(' ');
        }
        if c == ',' {
            result.push_str(", ");
            prev = ' ';
            continue;
        }
        result.push(c);
        prev = c;
    }
    result.trim().to_string()
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let block = rest.find("/*");
        let line = rest.find("//");
        let start = match (block, line) {
            (Some(b), Some(l)) => b.min(l),
            (Some(b), None) => b,
            (None, Some(l)) => l,
            (None, None) => break,
        };
        out.push_str(&rest[..start]);
        out.push(' ');
        let end = if rest[start..].starts_with("/*") {
            rest[start..].find("*/").map_or(rest.len(), |e| start + e + 2)
        } else {
            rest[start..].find('\n').map_or(rest.len(), |e| start + e)
        };
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}
