//! A C preprocessor macro expander, enough to expand kernel macros without
//! running the real preprocessor (which needs a `.config` and an architecture).
//!
//! It implements function-like macro expansion per C11 6.10.3 using Prosser's
//! hide-set algorithm: argument pre-expansion, `#` stringification, `##`
//! pasting with placemarkers, `__VA_ARGS__`, named variadic parameters
//! (`args...`), the GNU `, ## __VA_ARGS__` comma elision, and `__VA_OPT__`.
//! Object-like macros are deliberately left unexpanded so that kernel
//! annotations such as `notrace` stay visible to the extractor.
//!
//! A macro name can have several definitions (one per `#ifdef` branch or per
//! architecture). Without a configuration there is no way to know which one
//! applies, so [`expand_union`] expands every combination of the definitions
//! actually reached and returns all distinct results.

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Number,
    Str,
    Char,
    Punct,
    /// Stands in for an empty argument next to `##` (C11 6.10.3.3).
    Placemarker,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub text: Rc<str>,
    /// Whitespace preceded this token in the source.
    pub space_before: bool,
    /// Macros that must not be expanded again from this token.
    hide: Rc<Vec<Rc<str>>>,
    /// The real preprocessor would have replaced this token, or a part `##`
    /// built it from, with a value unknown here: an object-like macro or
    /// something like `__COUNTER__`. The text is not what the build sees.
    pub tainted: bool,
}

impl Token {
    fn new(kind: TokenKind, text: &str, space_before: bool) -> Token {
        Token {
            kind,
            text: Rc::from(text),
            space_before,
            hide: Rc::new(Vec::new()),
            tainted: false,
        }
    }

    pub fn is(&self, text: &str) -> bool {
        &*self.text == text
    }
}

/// One function-like macro definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MacroDef {
    pub params: Vec<String>,
    /// Name the variadic argument goes by in the body: `__VA_ARGS__` or the
    /// name of a GNU named variadic parameter (`args...`).
    pub variadic: Option<String>,
    /// Replacement text, comments removed.
    pub body: String,
    /// File and line that define it, reported as the origin of expansions.
    pub file: Arc<str>,
    pub line: u32,
    /// Architecture whose headers define it (`arch/<arch>/...`), if any. One
    /// expansion never mixes definitions from different architectures.
    pub arch: Option<Arc<str>>,
    /// Enclosing `#if` branches as (file, block, branch). One expansion takes
    /// every definition from the same branch of a given block, as a real
    /// build does.
    pub branches: Vec<Branch>,
}

impl MacroDef {
    /// `file:line`.
    pub fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// A branch of an `#if` block: (file, block number, branch number).
pub type Branch = (Arc<str>, u32, u32);

/// The definitions of a macro name visible at some point.
pub struct Resolved {
    /// Distinct function-like definitions, in a stable order.
    pub defs: Vec<Arc<MacroDef>>,
    /// Which definition applies cannot be determined (for example, a header
    /// redefines the macro, so it depends on include order).
    pub uncertain: bool,
}

/// Source of macro definitions.
pub trait Lookup {
    fn resolve(&self, name: &str) -> Resolved;

    /// Whether `name` is a visible object-like macro. Those are not expanded
    /// here, so where the real preprocessor would replace one, the text
    /// produced here differs from what the build sees.
    fn is_object_like(&self, _name: &str) -> bool {
        false
    }
}

/// Identifiers whose value only the compiler or the build knows.
const BUILD_TIME: &[&str] = &[
    "__COUNTER__",
    "__LINE__",
    "__FILE__",
    "__FILE_NAME__",
    "__BASE_FILE__",
    "__DATE__",
    "__TIME__",
    "__TIMESTAMP__",
    "__INCLUDE_LEVEL__",
    "KBUILD_MODNAME",
    "__KBUILD_MODNAME",
    "KBUILD_BASENAME",
    "KBUILD_MODFILE",
];

impl Lookup for HashMap<String, Vec<Arc<MacroDef>>> {
    fn resolve(&self, name: &str) -> Resolved {
        Resolved {
            defs: self.get(name).cloned().unwrap_or_default(),
            uncertain: false,
        }
    }
}

/// Limits that keep pathological macros from running away. Hitting one marks
/// the result incomplete rather than silently truncating it.
const MAX_EXPANSIONS: usize = 20_000;
const MAX_TOKENS: usize = 200_000;
pub const MAX_VARIANTS: usize = 64;

pub struct Variant {
    pub tokens: Vec<Token>,
    /// Architecture of the architecture-specific definitions used, if any,
    /// and the file of the first one.
    pub arch: Option<Arc<str>>,
    pub arch_def: Option<Arc<MacroDef>>,
    /// Every macro invocation expanded, with its arguments as written.
    pub expanded: Vec<(String, Vec<Token>)>,
    /// For macros that had several definitions: (macro, file of the one used).
    pub origins: Vec<(String, Arc<MacroDef>)>,
}

pub struct Union {
    /// Distinct expansions, one per combination of definitions reached.
    pub variants: Vec<Variant>,
    /// False when the result cannot be trusted: a limit was hit, a macro got
    /// the wrong number of arguments, or which definition applies is unknown.
    pub complete: bool,
    /// False when not every combination of definitions was expanded, so
    /// some names may be missing. Every name present is still a real result.
    pub exhaustive: bool,
    /// Why the result is not complete, one line per distinct reason.
    pub problems: Vec<String>,
    /// Every macro name looked up, defined or not, and every identifier
    /// checked for being an object-like macro. The result depends only on
    /// what the lookup says about these names, which makes it safe to cache.
    pub looked_up: Vec<String>,
    pub checked: Vec<String>,
}

/// Expands `input` under every combination of alternative definitions.
pub fn expand_union(input: &str, lookup: &dyn Lookup) -> Union {
    expand_union_in(input, lookup, &[])
}

/// Like `expand_union`, for text that sits inside the given `#if` branches:
/// definitions from other branches of those blocks are not used.
pub fn expand_union_in(input: &str, lookup: &dyn Lookup, context: &[Branch]) -> Union {
    let tokens = tokenize(input);
    let mut acc = Accumulator::default();

    // Phase 1, every combination. Each queued entry fixes some names to a
    // definition index; unfixed names use their first definition. Children
    // fix one more name that the parent run actually reached, so each
    // effective combination is visited once.
    let mut queue: VecDeque<HashMap<String, usize>> = VecDeque::from([HashMap::new()]);
    let mut runs = 0;
    while let Some(choices) = queue.pop_front() {
        if runs == MAX_VARIANTS {
            acc.exhaustive = false;
            break;
        }
        runs += 1;
        let multi = acc.run(&tokens, lookup, &choices, context);
        let mut fixed = choices.clone();
        for m in &multi {
            if choices.contains_key(&m.name) {
                continue;
            }
            for idx in 1..m.defs.len() {
                let mut child = fixed.clone();
                child.insert(m.name.clone(), idx);
                queue.push_back(child);
            }
            fixed.insert(m.name.clone(), 0);
        }
    }

    // Phase 2, when there are too many combinations: cover every alternative
    // definition of every reached macro at least once, keeping the choices
    // that made it reachable. Every result is still a consistent combination,
    // but names that need two specific non-default choices at once can be
    // missed, so the union is no longer exhaustive.
    if !acc.exhaustive {
        let mut scheduled: HashSet<(String, usize)> = acc.covered.clone();
        let mut queue: VecDeque<HashMap<String, usize>> = VecDeque::from([HashMap::new()]);
        let mut runs = 0;
        while let Some(choices) = queue.pop_front() {
            if runs == MAX_COVERING_RUNS {
                break;
            }
            runs += 1;
            let multi = acc.run(&tokens, lookup, &choices, context);
            let mut effective = choices.clone();
            for m in &multi {
                effective.insert(m.name.clone(), m.chosen);
            }
            for m in &multi {
                for (idx, def) in m.defs.iter().enumerate() {
                    if scheduled.insert((m.name.clone(), *def)) {
                        let mut child = effective.clone();
                        child.insert(m.name.clone(), idx);
                        queue.push_back(child);
                    }
                }
            }
        }
    }
    acc.finish()
}

const MAX_COVERING_RUNS: usize = 256;

/// A macro with several definitions, as reached in one run.
struct Multi {
    name: String,
    /// Identity of each alternative (pointer of its `Arc<MacroDef>`).
    defs: Vec<usize>,
    chosen: usize,
}

struct Accumulator {
    variants: Vec<Variant>,
    seen_text: HashSet<String>,
    looked_up: HashSet<String>,
    complete: bool,
    exhaustive: bool,
    problems: Vec<String>,
    impossible: Vec<String>,
    /// (macro, alternative) pairs some run has used.
    covered: HashSet<(String, usize)>,
    checked: HashSet<String>,
}

impl Default for Accumulator {
    fn default() -> Self {
        Accumulator {
            variants: Vec::new(),
            seen_text: HashSet::new(),
            looked_up: HashSet::new(),
            complete: true,
            exhaustive: true,
            problems: Vec::new(),
            impossible: Vec::new(),
            covered: HashSet::new(),
            checked: HashSet::new(),
        }
    }
}

impl Accumulator {
    /// Expands once under `choices`; returns the multi-definition macros reached.
    fn run(
        &mut self,
        tokens: &[Token],
        lookup: &dyn Lookup,
        choices: &HashMap<String, usize>,
        context: &[Branch],
    ) -> Vec<Multi> {
        let mut ex = Expander::new(lookup, choices);
        for (file, block, branch) in context {
            ex.branches.insert((file.clone(), *block), *branch);
        }
        let out = ex.expand(tokens.to_vec());
        self.complete &= !ex.incomplete;
        for p in ex.problems.drain(..) {
            if !self.problems.contains(&p) {
                self.problems.push(p);
            }
        }
        self.looked_up.extend(ex.looked_up.iter().cloned());
        self.checked.extend(ex.checked.borrow_mut().drain());
        for m in &ex.multi {
            self.covered.insert((m.name.clone(), m.defs[m.chosen]));
        }
        let multi = std::mem::take(&mut ex.multi);
        // A combination that gives some macro the wrong number of arguments
        // is not one any real configuration uses; drop it.
        if !ex.invalid.is_empty() {
            for p in ex.invalid {
                if !self.impossible.contains(&p) {
                    self.impossible.push(p);
                }
            }
        } else if self.seen_text.insert(render(&out)) {
            self.variants.push(Variant {
                tokens: out,
                arch: ex.arch.clone(),
                arch_def: ex.arch_def.clone(),
                expanded: ex.expanded,
                origins: ex.origins,
            });
        }
        multi
    }

    fn finish(mut self) -> Union {
        if self.variants.is_empty() && !self.impossible.is_empty() {
            self.complete = false;
            self.problems.extend(self.impossible);
        }
        let mut looked_up: Vec<String> = self.looked_up.into_iter().collect();
        looked_up.sort();
        let mut checked: Vec<String> = self.checked.into_iter().collect();
        checked.sort();
        Union {
            checked,
            variants: self.variants,
            complete: self.complete,
            exhaustive: self.exhaustive,
            problems: self.problems,
            looked_up,
        }
    }
}

struct Expander<'a> {
    lookup: &'a dyn Lookup,
    choices: &'a HashMap<String, usize>,
    /// Names with several definitions reached in this run.
    multi: Vec<Multi>,
    origins: Vec<(String, Arc<MacroDef>)>,
    looked_up: HashSet<String>,
    expansions: usize,
    incomplete: bool,
    problems: Vec<String>,
    /// Why this combination of definitions is impossible, if it is.
    invalid: Vec<String>,
    /// Architecture fixed by the first architecture-specific definition used.
    arch: Option<Arc<str>>,
    expanded: Vec<(String, Vec<Token>)>,
    checked: std::cell::RefCell<HashSet<String>>,
    /// Branch taken in each `#if` block used so far.
    branches: HashMap<(Arc<str>, u32), u32>,
    arch_def: Option<Arc<MacroDef>>,
}

impl<'a> Expander<'a> {
    fn new(lookup: &'a dyn Lookup, choices: &'a HashMap<String, usize>) -> Self {
        Expander {
            lookup,
            choices,
            multi: Vec::new(),
            origins: Vec::new(),
            looked_up: HashSet::new(),
            expansions: 0,
            incomplete: false,
            problems: Vec::new(),
            invalid: Vec::new(),
            arch: None,
            expanded: Vec::new(),
            checked: std::cell::RefCell::new(HashSet::new()),
            branches: HashMap::new(),
            arch_def: None,
        }
    }

    fn pick(&mut self, name: &str) -> Option<Arc<MacroDef>> {
        self.looked_up.insert(name.to_string());
        let Resolved { defs, uncertain } = self.lookup.resolve(name);
        let defs: Vec<Arc<MacroDef>> = defs
            .into_iter()
            .filter(|d| self.arch.is_none() || d.arch.is_none() || d.arch == self.arch)
            .filter(|d| {
                d.branches
                    .iter()
                    .all(|(f, b, br)| self.branches.get(&(f.clone(), *b)).is_none_or(|taken| taken == br))
            })
            .collect();
        if defs.is_empty() {
            return None;
        }
        if uncertain {
            self.flag(format!("`{name}` is redefined in an included header"));
        }
        let idx = self.choices.get(name).copied().unwrap_or(0).min(defs.len() - 1);
        if defs.len() > 1 && !self.multi.iter().any(|m| m.name == name) {
            self.multi.push(Multi {
                name: name.to_string(),
                defs: defs.iter().map(|d| Arc::as_ptr(d) as usize).collect(),
                chosen: idx,
            });
            self.origins.push((name.to_string(), defs[idx].clone()));
        }
        if self.arch.is_none() && defs[idx].arch.is_some() {
            self.arch = defs[idx].arch.clone();
            self.arch_def = Some(defs[idx].clone());
        }
        for (f, b, br) in &defs[idx].branches {
            self.branches.insert((f.clone(), *b), *br);
        }
        Some(defs[idx].clone())
    }

    fn flag(&mut self, problem: String) {
        self.incomplete = true;
        if !self.problems.contains(&problem) {
            self.problems.push(problem);
        }
    }

    /// Taints an identifier the real preprocessor would replace at this
    /// point (it is being rescanned, not pasted).
    fn mark_unknown(&self, mut t: Token) -> Token {
        self.checked.borrow_mut().insert(t.text.to_string());
        if BUILD_TIME.contains(&&*t.text) || t.text.starts_with("CONFIG_") || self.lookup.is_object_like(&t.text) {
            t.tainted = true;
        }
        t
    }

    /// Pastes `rhs` onto the last token of `out` (C11 6.10.3.3).
    fn paste(&self, out: &mut Vec<Token>, rhs: Token) {
        let Some(lhs) = out.pop() else {
            out.push(rhs);
            return;
        };
        if lhs.kind == TokenKind::Placemarker {
            out.push(rhs);
            return;
        }
        if rhs.kind == TokenKind::Placemarker {
            out.push(lhs);
            return;
        }
        // `##` operands are not macro-expanded, so only parts that were
        // already replaced by the real preprocessor make the result unknown.
        let tainted = lhs.tainted || rhs.tainted;
        let joined = format!("{}{}", lhs.text, rhs.text);
        let mut toks = tokenize(&joined);
        if toks.len() == 1 {
            let mut t = toks.pop().expect("one token");
            t.space_before = lhs.space_before;
            t.tainted = tainted;
            out.push(t);
        } else {
            // Not a valid preprocessing token; GCC rejects this. Keep both.
            out.push(lhs);
            out.push(rhs);
        }
    }

    /// Fully macro-expands a token list (C11 6.10.3.4 rescanning).
    fn expand(&mut self, tokens: Vec<Token>) -> Vec<Token> {
        let mut input: VecDeque<Token> = tokens.into();
        let mut out = Vec::new();
        while let Some(t) = input.pop_front() {
            if self.expansions >= MAX_EXPANSIONS || out.len() + input.len() > MAX_TOKENS {
                self.flag("expansion too large".to_string());
                out.push(t);
                out.extend(input);
                break;
            }
            if t.kind != TokenKind::Ident || t.hide.iter().any(|h| **h == *t.text) {
                out.push(t);
                continue;
            }
            // Only an identifier followed by `(` invokes a function-like macro.
            if !input.front().is_some_and(|n| n.is("(")) {
                out.push(self.mark_unknown(t));
                continue;
            }
            let Some(def) = self.pick(&t.text) else {
                out.push(self.mark_unknown(t));
                continue;
            };
            let Some((args, rparen, consumed)) = collect_args(&mut input) else {
                out.push(t);
                continue;
            };
            let Some(args) = match_args(&def, args) else {
                // Wrong number of arguments: the real preprocessor would stop
                // with an error, so no configuration produces this.
                let problem = format!("`{}` given the wrong number of arguments", t.text);
                if !self.invalid.contains(&problem) {
                    self.invalid.push(problem);
                }
                for tok in consumed.into_iter().rev() {
                    input.push_front(tok);
                }
                out.push(t);
                continue;
            };
            self.expansions += 1;
            self.expanded
                .push((t.text.to_string(), args.iter().flatten().cloned().collect()));
            let mut hide: Vec<Rc<str>> = t.hide.iter().filter(|h| rparen.hide.contains(h)).cloned().collect();
            hide.push(t.text.clone());
            hide.sort();
            hide.dedup();
            let mut body = self.substitute(&def, &args, &Rc::new(hide));
            if let Some(first) = body.first_mut() {
                first.space_before = t.space_before;
            }
            for tok in body.into_iter().rev() {
                input.push_front(tok);
            }
        }
        out
    }

    /// Argument substitution (C11 6.10.3.1-3).
    fn substitute(&mut self, def: &MacroDef, args: &[Vec<Token>], hide: &Rc<Vec<Rc<str>>>) -> Vec<Token> {
        let body = tokenize(&def.body);
        let mut out = self.substitute_tokens(def, &body, args);
        out.retain(|t| t.kind != TokenKind::Placemarker);
        for t in &mut out {
            let mut h: Vec<Rc<str>> = t.hide.iter().cloned().collect();
            h.extend(hide.iter().cloned());
            h.sort();
            h.dedup();
            t.hide = Rc::new(h);
        }
        out
    }

    fn substitute_tokens(&mut self, def: &MacroDef, body: &[Token], args: &[Vec<Token>]) -> Vec<Token> {
        let param = |t: &Token| -> Option<usize> {
            if t.kind != TokenKind::Ident {
                return None;
            }
            def.params
                .iter()
                .position(|p| **p == *t.text)
                .or_else(|| (def.variadic.as_deref() == Some(&*t.text)).then_some(def.params.len()))
        };
        let va_index = def.variadic.as_ref().map(|_| def.params.len());
        let mut expanded: HashMap<usize, Vec<Token>> = HashMap::new();
        let mut out: Vec<Token> = Vec::new();
        let mut i = 0;
        while i < body.len() {
            let t = &body[i];

            if t.is("__VA_OPT__")
                && va_index.is_some()
                && body.get(i + 1).is_some_and(|n| n.is("("))
                && let Some(close) = matching(body, i + 1)
            {
                let va_present = va_index.is_some_and(|v| !args[v].is_empty());
                if va_present {
                    let inner = self.substitute_tokens(def, &body[i + 2..close], args);
                    out.extend(inner);
                }
                i = close + 1;
                continue;
            }

            if t.is("#")
                && let Some(p) = body.get(i + 1).and_then(param)
            {
                out.push(stringify(&args[p], t.space_before));
                i += 2;
                continue;
            }

            if t.is("##") && i + 1 < body.len() {
                let next = &body[i + 1];
                match param(next) {
                    Some(p) if Some(p) == va_index && out.last().is_some_and(|l| l.is(",")) => {
                        // GNU extension: `, ## __VA_ARGS__` drops the comma when
                        // there are no variadic arguments, and does not paste.
                        if args[p].is_empty() {
                            out.pop();
                        } else {
                            out.extend(args[p].iter().cloned());
                        }
                    }
                    Some(p) => {
                        let arg = &args[p];
                        match arg.split_first() {
                            None => self.paste(&mut out, placemarker()),
                            Some((first, rest)) => {
                                self.paste(&mut out, first.clone());
                                out.extend(rest.iter().cloned());
                            }
                        }
                    }
                    None => self.paste(&mut out, next.clone()),
                }
                i += 2;
                continue;
            }

            if let Some(p) = param(t) {
                let before_paste = body.get(i + 1).is_some_and(|n| n.is("##"));
                let mut toks = if before_paste {
                    // Operands of `##` are not macro-expanded.
                    if args[p].is_empty() {
                        vec![placemarker()]
                    } else {
                        args[p].clone()
                    }
                } else {
                    expanded.entry(p).or_insert_with(|| self.expand(args[p].clone()));
                    expanded[&p].clone()
                };
                if let Some(first) = toks.first_mut() {
                    first.space_before = t.space_before;
                }
                out.extend(toks);
                i += 1;
                continue;
            }

            out.push(t.clone());
            i += 1;
        }
        out
    }
}

fn placemarker() -> Token {
    Token::new(TokenKind::Placemarker, "", false)
}

fn stringify(arg: &[Token], space_before: bool) -> Token {
    let mut s = String::from("\"");
    for (i, t) in arg.iter().enumerate() {
        if i > 0 && t.space_before {
            s.push(' ');
        }
        if matches!(t.kind, TokenKind::Str | TokenKind::Char) {
            for c in t.text.chars() {
                if c == '"' || c == '\\' {
                    s.push('\\');
                }
                s.push(c);
            }
        } else {
            s.push_str(&t.text);
        }
    }
    s.push('"');
    Token::new(TokenKind::Str, &s, space_before)
}

/// Index of the `)` matching the `(` at `open`.
fn matching(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Takes `( args )` off the front of `input`. Returns the arguments split at
/// top-level commas, the closing parenthesis, and every token consumed.
fn collect_args(input: &mut VecDeque<Token>) -> Option<(Vec<Vec<Token>>, Token, Vec<Token>)> {
    let mut consumed = Vec::new();
    let mut args = vec![Vec::new()];
    let mut depth = 0usize;
    while let Some(t) = input.pop_front() {
        consumed.push(t.clone());
        if t.is("(") {
            depth += 1;
            if depth == 1 {
                continue;
            }
        } else if t.is(")") {
            depth -= 1;
            if depth == 0 {
                return Some((args, t, consumed));
            }
        } else if t.is(",") && depth == 1 {
            args.push(Vec::new());
            continue;
        }
        args.last_mut().expect("at least one argument").push(t);
    }
    for t in consumed.into_iter().rev() {
        input.push_front(t);
    }
    None
}

/// Maps invocation arguments onto parameters; the variadic argument, if any,
/// comes last and keeps its commas.
fn match_args(def: &MacroDef, mut args: Vec<Vec<Token>>) -> Option<Vec<Vec<Token>>> {
    let n = def.params.len();
    if args.len() == 1 && args[0].is_empty() && n == 0 {
        args.clear();
    }
    if def.variadic.is_none() {
        return (args.len() == n).then_some(args);
    }
    if args.len() < n {
        return None;
    }
    let rest = args.split_off(n);
    let mut va = Vec::new();
    for (i, a) in rest.into_iter().enumerate() {
        if i > 0 {
            va.push(Token::new(TokenKind::Punct, ",", false));
        }
        va.extend(a);
    }
    args.push(va);
    Some(args)
}

const PUNCTUATORS: &[&str] = &[
    "...", "<<=", ">>=", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "*=", "/=", "%=", "+=",
    "-=", "&=", "^=", "|=", "##",
];

/// Splits text into preprocessing tokens. Backslash-newlines are removed and
/// comments must already be stripped.
pub fn tokenize(text: &str) -> Vec<Token> {
    let text = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut space = false;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        let start = i;
        let kind = if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            TokenKind::Ident
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            i += 1;
            while i < b.len() {
                let exponent_sign = (b[i] == b'+' || b[i] == b'-') && matches!(b[i - 1], b'e' | b'E' | b'p' | b'P');
                if exponent_sign || b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.' {
                    i += 1;
                } else {
                    break;
                }
            }
            TokenKind::Number
        } else if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != c && b[i] != b'\n' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            if c == b'"' { TokenKind::Str } else { TokenKind::Char }
        } else {
            let p = PUNCTUATORS.iter().find(|p| text[i..].starts_with(**p));
            i += p.map_or_else(|| text[i..].chars().next().map_or(1, char::len_utf8), |p| p.len());
            TokenKind::Punct
        };
        out.push(Token::new(kind, &text[start..i], space));
        space = false;
    }
    out
}

/// Joins tokens back into text, keeping a space wherever the source had one
/// or two words would otherwise merge.
pub fn render(tokens: &[Token]) -> String {
    let mut s = String::new();
    let mut prev_word = false;
    for t in tokens {
        let word = matches!(t.kind, TokenKind::Ident | TokenKind::Number);
        if !s.is_empty() && (t.space_before || (word && prev_word)) {
            s.push(' ');
        }
        s.push_str(&t.text);
        prev_word = word;
    }
    s
}

/// Renders expanded top-level code in kernel layout: each function body that
/// follows a declarator is replaced by `{` and `}` on their own lines, and
/// top-level statements end their line. The extractor relies on that layout
/// to stay robust against whatever the bodies contain.
pub fn render_for_extraction(tokens: &[Token]) -> String {
    let mut out: Vec<Token> = Vec::new();
    let mut s = String::new();
    let mut i = 0;
    let mut paren_depth = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        if t.is("(") {
            paren_depth += 1;
        } else if t.is(")") {
            paren_depth = paren_depth.saturating_sub(1);
        }
        if t.is("{") && paren_depth == 0 && out.last().is_some_and(|l| l.is(")")) {
            s.push_str(&render(&out));
            out.clear();
            // Skip the body.
            let mut depth = 0usize;
            while i < tokens.len() {
                if tokens[i].is("{") {
                    depth += 1;
                } else if tokens[i].is("}") {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            s.push_str("\n{\n}\n");
            i += 1;
            continue;
        }
        out.push(t.clone());
        if t.is(";") && paren_depth == 0 {
            s.push_str(&render(&out));
            s.push('\n');
            out.clear();
        }
        i += 1;
    }
    s.push_str(&render(&out));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(defs: &[(&str, &str)]) -> HashMap<String, Vec<Arc<MacroDef>>> {
        let mut t: HashMap<String, Vec<Arc<MacroDef>>> = HashMap::new();
        for (i, (head, body)) in defs.iter().enumerate() {
            let open = head.find('(').unwrap();
            let name = &head[..open];
            let mut params = Vec::new();
            let mut variadic = None;
            for p in head[open + 1..head.len() - 1]
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                match p.strip_suffix("...") {
                    Some("") => variadic = Some("__VA_ARGS__".to_string()),
                    Some(n) => variadic = Some(n.to_string()),
                    None => params.push(p.to_string()),
                }
            }
            t.entry(name.to_string()).or_default().push(Arc::new(MacroDef {
                params,
                variadic,
                body: body.to_string(),
                file: Arc::from(format!("file{i}.h")),
                line: 1,
                arch: None,
                branches: Vec::new(),
            }));
        }
        t
    }

    fn one(input: &str, defs: &[(&str, &str)]) -> String {
        let u = expand_union(input, &table(defs));
        assert!(u.complete);
        assert_eq!(u.variants.len(), 1, "expected a single variant");
        render(&u.variants[0].tokens)
    }

    #[test]
    fn standard_examples() {
        // C11 6.10.3.5 example 3, the parts that use only function-like macros.
        let defs = [("f(a)", "f(x * (a))"), ("g(x)", "f"), ("t(a)", "a"), ("m(a)", "a(w)")];
        assert_eq!(one("f(y+1)", &[("f(a)", "x * (a)")]), "x * (y+1)");
        // Self-reference is not re-expanded.
        assert_eq!(one("f(2)", &defs), "f(x * (2))");
        assert_eq!(one("t(t)(1)", &[("t(a)", "a")]), "t(1)");
        assert_eq!(one("m(m)", &defs), "m(w)");
    }

    #[test]
    fn stringify_and_paste() {
        assert_eq!(one("str(a \"b\" 'c')", &[("str(x)", "#x")]), r#""a \"b\" 'c'""#);
        assert_eq!(
            one("cat(meta_, int_random)", &[("cat(a, b)", "a ## b")]),
            "meta_int_random"
        );
        assert_eq!(one("cat(, x)", &[("cat(a, b)", "a ## b")]), "x");
        assert_eq!(one("cat(x, )", &[("cat(a, b)", "a ## b")]), "x");
        // `##` operands are not expanded first; the result is rescanned.
        let defs = [("cat(a, b)", "a ## b"), ("xy()", "done"), ("x()", "nope")];
        assert_eq!(one("cat(x, y)()", &defs), "done");
    }

    #[test]
    fn pasting_unknown_values_taints_the_name() {
        // __initcall_stub-style names depend on __COUNTER__ and the module name.
        let defs = [
            ("__PASTE(a, b)", "___PASTE(a, b)"),
            ("___PASTE(a, b)", "a##b"),
            ("stub(fn)", "int __PASTE(__initstub_, __PASTE(__COUNTER__, fn))(void)"),
        ];
        let u = expand_union("stub(foo_init)", &table(&defs));
        let name = u.variants[0]
            .tokens
            .iter()
            .find(|t| t.text.starts_with("__initstub"))
            .unwrap();
        assert!(name.tainted);
        let u = expand_union("__PASTE(foo_, bar)", &table(&defs));
        assert!(!u.variants[0].tokens[0].tainted);
        // Direct `##` pastes its operand literally, even a builtin.
        let u = expand_union("cat(x, __LINE__)", &table(&[("cat(a, b)", "a##b")]));
        assert_eq!(render(&u.variants[0].tokens), "x__LINE__");
        assert!(!u.variants[0].tokens[0].tainted);
    }

    #[test]
    fn variadic() {
        let defs = [
            ("pr(fmt, ...)", "printk(fmt, ##__VA_ARGS__)"),
            ("prn(fmt, args...)", "printk(fmt, ## args)"),
            ("all(...)", "call(__VA_ARGS__)"),
            ("opt(a, ...)", "f(a __VA_OPT__(,) __VA_ARGS__)"),
        ];
        assert_eq!(one("pr(\"x\")", &defs), "printk(\"x\")");
        assert_eq!(one("pr(\"x\", 1, 2)", &defs), "printk(\"x\", 1, 2)");
        assert_eq!(one("prn(\"x\")", &defs), "printk(\"x\")");
        assert_eq!(one("prn(\"x\", y)", &defs), "printk(\"x\", y)");
        assert_eq!(one("all(a, (b, c), d)", &defs), "call(a, (b, c), d)");
        assert_eq!(one("all()", &defs), "call()");
        assert_eq!(one("opt(1)", &defs), "f(1)");
        assert_eq!(one("opt(1, 2)", &defs), "f(1, 2)");
    }

    #[test]
    fn arguments_are_expanded_before_substitution() {
        let defs = [("id(x)", "x"), ("two()", "2"), ("wrap(x)", "[x]")];
        assert_eq!(one("wrap(id(two()))", &defs), "[2]");
    }

    #[test]
    fn identifier_without_parenthesis_is_not_an_invocation() {
        assert_eq!(one("f + f(1)", &[("f(x)", "<x>")]), "f + <1>");
    }

    #[test]
    fn wrong_argument_count_means_no_configuration() {
        let u = expand_union("f(1, 2)", &table(&[("f(x)", "<x>")]));
        assert!(!u.complete);
        assert!(u.variants.is_empty());
        assert_eq!(u.problems, ["`f` given the wrong number of arguments"]);
    }

    #[test]
    fn impossible_combinations_are_dropped() {
        // Only the second definition of B takes two arguments.
        let u = expand_union(
            "A()",
            &table(&[("A()", "B(1, 2)"), ("B(x)", "one"), ("B(x, y)", "two")]),
        );
        assert!(u.complete);
        assert_eq!(u.variants.len(), 1);
        assert_eq!(render(&u.variants[0].tokens), "two");
    }

    #[test]
    fn one_architecture_per_expansion() {
        let mut t = table(&[("TOP(x)", "WRAP(x) ARG(x)")]);
        for (name, arch, body) in [
            ("WRAP", "x86", "x86_wrap"),
            ("WRAP", "s390", "s390_wrap"),
            ("ARG", "x86", "x86_arg"),
            ("ARG", "s390", "s390_arg"),
        ] {
            t.entry(name.to_string()).or_default().push(Arc::new(MacroDef {
                params: vec!["x".into()],
                variadic: None,
                body: body.into(),
                file: Arc::from(format!("arch/{arch}/include/asm/h.h")),
                line: 1,
                arch: Some(Arc::from(arch)),
                branches: Vec::new(),
            }));
        }
        let u = expand_union("TOP(1)", &t);
        let mut out: Vec<String> = u.variants.iter().map(|v| render(&v.tokens)).collect();
        out.sort();
        assert_eq!(out, ["s390_wrap s390_arg", "x86_wrap x86_arg"]);
    }

    #[test]
    fn union_over_alternative_definitions() {
        // Like __SYSCALL_DEFINEx: one definition per architecture.
        let defs = [
            ("DEF(name)", "STUB(name) long sys_##name(void)"),
            ("STUB(name)", "long __x64_sys_##name(void);"),
            ("STUB(name)", ""),
        ];
        let u = expand_union("DEF(read)", &table(&defs));
        assert!(u.complete);
        let mut out: Vec<String> = u.variants.iter().map(|v| render(&v.tokens)).collect();
        out.sort();
        assert_eq!(
            out,
            ["long __x64_sys_read(void); long sys_read(void)", "long sys_read(void)"]
        );
        let origins: Vec<&str> = u.variants.iter().map(|v| &*v.origins[0].1.file).collect();
        assert!(origins.contains(&"file1.h") && origins.contains(&"file2.h"));
        assert!(u.looked_up.contains(&"STUB".to_string()));
    }

    #[test]
    fn union_reaches_definitions_behind_other_choices() {
        // B is only reached when A uses its second definition.
        let defs = [("A()", "x"), ("A()", "B()"), ("B()", "b1"), ("B()", "b2")];
        let u = expand_union("A()", &table(&defs));
        let mut out: Vec<String> = u.variants.iter().map(|v| render(&v.tokens)).collect();
        out.sort();
        assert_eq!(out, ["b1", "b2", "x"]);
    }

    #[test]
    fn many_combinations_fall_back_to_covering_every_alternative() {
        // 7 independent macros with 2 definitions each: 128 combinations.
        let mut defs: Vec<(String, String)> = vec![(
            "TOP()".into(),
            (0..7).map(|i| format!("M{i}()")).collect::<Vec<_>>().join(" "),
        )];
        for i in 0..7 {
            defs.push((format!("M{i}()"), format!("a{i}")));
            defs.push((format!("M{i}()"), format!("b{i}")));
        }
        let refs: Vec<(&str, &str)> = defs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let u = expand_union("TOP()", &table(&refs));
        assert!(u.complete, "results are trustworthy");
        assert!(!u.exhaustive);
        let all: String = u
            .variants
            .iter()
            .map(|v| render(&v.tokens))
            .collect::<Vec<_>>()
            .join(" ");
        for i in 0..7 {
            assert!(
                all.contains(&format!("a{i}")) && all.contains(&format!("b{i}")),
                "alternative {i} covered"
            );
        }
    }

    #[test]
    fn definitions_from_one_ifdef_block_are_chosen_together() {
        // include/linux/init.h defines both stubs inside one
        // #ifdef CONFIG_LTO_CLANG ... #else ... #endif.
        let f: Arc<str> = Arc::from("include/linux/init.h");
        let def = |params: &[&str], body: &str, branch: u32| {
            Arc::new(MacroDef {
                params: params.iter().map(|p| p.to_string()).collect(),
                variadic: None,
                body: body.into(),
                file: f.clone(),
                line: 1,
                arch: None,
                branches: vec![(f.clone(), 7, branch)],
            })
        };
        let mut t: HashMap<String, Vec<Arc<MacroDef>>> = HashMap::new();
        t.insert(
            "NAME".into(),
            vec![def(&["fn"], "__initstub_##fn", 0), def(&["fn"], "fn", 1)],
        );
        t.insert(
            "STUB".into(),
            vec![
                def(&["s", "fn"], "int s(void) { return fn(); }", 0),
                def(&["s", "fn"], "ADDR(fn)", 1),
            ],
        );
        let u = expand_union("STUB(NAME(foo), foo)", &t);
        let mut out: Vec<String> = u.variants.iter().map(|v| render(&v.tokens)).collect();
        out.sort();
        assert_eq!(out, ["ADDR(foo)", "int __initstub_foo(void) { return foo(); }"]);

        // Text inside the #else branch only sees that branch's definitions.
        let u = expand_union_in("STUB(NAME(foo), foo)", &t, &[(f.clone(), 7, 1)]);
        assert_eq!(u.variants.len(), 1);
        assert_eq!(render(&u.variants[0].tokens), "ADDR(foo)");
    }

    #[test]
    fn runaway_expansion_is_flagged() {
        let defs = [("a(x)", "b(x x)"), ("b(x)", "a(x x)")];
        let u = expand_union("a(1)", &table(&defs));
        // Mutual recursion stops through hide sets, not limits.
        assert!(u.complete);
        assert_eq!(render(&u.variants[0].tokens), "a(1 1 1 1)");
    }

    #[test]
    fn layout_for_extraction() {
        let toks = tokenize("static int f(int a) { if (a) { return 1; } return 0; } int g(void);");
        assert_eq!(
            render_for_extraction(&toks),
            "static int f(int a)\n{\n}\nint g(void);\n"
        );
    }
}
