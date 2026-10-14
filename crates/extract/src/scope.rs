//! Which macro definitions are visible at a point in a kernel source file.
//!
//! The real preprocessor sees the macros of the file itself and of the
//! headers it includes, transitively. This module resolves `#include`s the
//! way the kernel build does, except that with no architecture chosen
//! `<asm/...>` resolves to every architecture's header plus `asm-generic`,
//! so the result is the union over architectures.

use crate::FileFacts;
use crate::cpp::{self, Lookup, MacroDef, Resolved};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// C keywords are never treated as macros. The kernel defines `if` as a
/// macro only under CONFIG_PROFILE_ALL_BRANCHES; expanding it everywhere
/// would be wrong for every other configuration.
const KEYWORDS: &[&str] = &[
    "if", "else", "while", "for", "do", "switch", "case", "default", "return", "break", "continue", "goto", "sizeof",
    "typeof", "struct", "union", "enum", "static", "inline", "const", "volatile", "extern", "register", "auto",
];

struct Def {
    file: usize,
    line: u32,
    /// Identical definitions share a key, so they count as one alternative.
    key: Arc<str>,
    /// Still defined at the end of its file, so files that include it see it.
    /// A header's private helper (define, use, `#undef`) is not.
    exported: bool,
    def: Arc<MacroDef>,
}

pub struct Scope {
    paths: Vec<Arc<str>>,
    defs: HashMap<String, Vec<Def>>,
    undefs: HashMap<String, Vec<(usize, u32)>>,
    /// Files defining each object-like macro name.
    object_like: HashMap<String, Vec<usize>>,
    /// Files that `#undef` a name they did not define themselves, i.e. that
    /// replace someone else's definition. Which definition an includer sees
    /// then depends on include order.
    foreign_undefs: HashMap<String, Vec<usize>>,
    /// Resolved `#include` targets of each file.
    edges: Vec<Vec<usize>>,
    /// Strongly connected component of each file in the include graph.
    comp: Vec<usize>,
    comp_members: Vec<Vec<usize>>,
    comp_edges: Vec<Vec<usize>>,
    /// Files included by something get a bit in the visibility sets.
    bit: Vec<Option<usize>>,
    words: usize,
    closure: Vec<OnceLock<Arc<Vec<u64>>>>,
}

impl Scope {
    pub fn new(files: &[(Arc<str>, &FileFacts)]) -> Scope {
        let paths: Vec<Arc<str>> = files.iter().map(|(p, _)| p.clone()).collect();
        let by_path: HashMap<&str, usize> = paths.iter().enumerate().map(|(i, p)| (&**p, i)).collect();
        let arches: Vec<&str> = {
            let mut a: Vec<&str> = paths
                .iter()
                .filter_map(|p| p.strip_prefix("arch/")?.split('/').next())
                .collect();
            a.sort();
            a.dedup();
            a
        };

        let edges: Vec<Vec<usize>> = files
            .iter()
            .map(|(path, facts)| {
                let mut out: Vec<usize> = facts
                    .includes
                    .iter()
                    .flat_map(|inc| resolve_include(path, inc, &by_path, &arches))
                    .collect();
                out.sort();
                out.dedup();
                out
            })
            .collect();

        let mut bit = vec![None; files.len()];
        let mut words = 0usize;
        for &t in edges.iter().flatten() {
            if bit[t].is_none() {
                bit[t] = Some(words);
                words += 1;
            }
        }
        let words = words.div_ceil(64);

        let (comp, comp_members) = tarjan(&edges);
        let mut comp_edges = vec![Vec::new(); comp_members.len()];
        for (from, targets) in edges.iter().enumerate() {
            for &to in targets {
                if comp[from] != comp[to] {
                    comp_edges[comp[from]].push(comp[to]);
                }
            }
        }
        for e in &mut comp_edges {
            e.sort();
            e.dedup();
        }

        let mut defs: HashMap<String, Vec<Def>> = HashMap::new();
        let mut undefs: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
        let mut foreign_undefs: HashMap<String, Vec<usize>> = HashMap::new();
        let mut object_like: HashMap<String, Vec<usize>> = HashMap::new();
        for (id, (path, facts)) in files.iter().enumerate() {
            for name in &facts.object_macros {
                object_like.entry(name.clone()).or_default().push(id);
            }
            // Per name, this file's directives in order: (line, is_define).
            let mut events: HashMap<&str, Vec<(u32, bool)>> = HashMap::new();
            for m in &facts.macros {
                events.entry(&m.name).or_default().push((m.line, true));
            }
            for (name, line) in &facts.undefs {
                events.entry(name).or_default().push((*line, false));
            }
            // Definitions after the file's last `#undef` of a name survive to
            // the end of the file. There can be several, in `#ifdef` branches.
            let mut last_undef: HashMap<&str, u32> = HashMap::new();
            for (name, ev) in &mut events {
                ev.sort();
                let mut defined = false;
                for &(_, is_define) in ev.iter() {
                    if is_define {
                        defined = true;
                    } else {
                        if !defined {
                            foreign_undefs.entry(name.to_string()).or_default().push(id);
                        }
                        defined = false;
                    }
                }
                if let Some(&(line, _)) = ev.iter().rev().find(|e| !e.1) {
                    last_undef.insert(name, line);
                }
            }
            let arch: Option<Arc<str>> = path
                .strip_prefix("arch/")
                .and_then(|r| r.split('/').next())
                .map(Arc::from);
            for m in &facts.macros {
                let key = format!(
                    "{:?}\0{:?}\0{}",
                    m.params,
                    m.variadic,
                    cpp::tokenize(&m.body)
                        .iter()
                        .map(|t| &*t.text)
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                defs.entry(m.name.clone()).or_default().push(Def {
                    file: id,
                    line: m.line,
                    key: Arc::from(key),
                    exported: last_undef.get(m.name.as_str()).is_none_or(|&u| m.line > u),
                    def: Arc::new(MacroDef {
                        params: m.params.clone(),
                        variadic: m.variadic.clone(),
                        body: m.body.clone(),
                        file: path.clone(),
                        line: m.line,
                        arch: arch.clone(),
                        branches: m.conditions.iter().map(|c| (path.clone(), c.block, c.branch)).collect(),
                    }),
                });
            }
            for (name, line) in &facts.undefs {
                undefs.entry(name.clone()).or_default().push((id, *line));
            }
        }

        let closure = (0..comp_members.len()).map(|_| OnceLock::new()).collect();
        Scope {
            paths,
            defs,
            undefs,
            object_like,
            foreign_undefs,
            edges,
            comp,
            comp_members,
            comp_edges,
            bit,
            words,
            closure,
        }
    }

    pub fn file_id(&self, path: &str) -> Option<usize> {
        self.paths.iter().position(|p| &**p == path)
    }

    /// Files reachable through the includes of a component, as a bitset.
    fn comp_closure(&self, c: usize) -> Arc<Vec<u64>> {
        if let Some(done) = self.closure[c].get() {
            return done.clone();
        }
        let mut set = vec![0u64; self.words];
        for &m in &self.comp_members[c] {
            if let Some(b) = self.bit[m] {
                set[b / 64] |= 1 << (b % 64);
            }
        }
        for &child in &self.comp_edges[c] {
            for (w, v) in set.iter_mut().zip(self.comp_closure(child).iter()) {
                *w |= v;
            }
        }
        self.closure[c].get_or_init(|| Arc::new(set)).clone()
    }

    /// The macro environment at `line` of `file`.
    pub fn at(&self, file: usize, line: u32) -> Context<'_> {
        Context {
            scope: self,
            file,
            line,
            visible: self.comp_closure(self.comp[file]),
        }
    }

    /// Every distinct name that has a function-like macro definition.
    pub fn is_macro(&self, name: &str) -> bool {
        self.defs.contains_key(name)
    }

    /// The expander's form of a definition in `file`.
    pub fn macro_def(&self, file: usize, m: &crate::MacroDefinition) -> Arc<MacroDef> {
        let path = &self.paths[file];
        Arc::new(MacroDef {
            params: m.params.clone(),
            variadic: m.variadic.clone(),
            body: m.body.clone(),
            file: path.clone(),
            line: m.line,
            arch: path
                .strip_prefix("arch/")
                .and_then(|r| r.split('/').next())
                .map(Arc::from),
            branches: m.conditions.iter().map(|c| (path.clone(), c.block, c.branch)).collect(),
        })
    }

    pub fn path(&self, file: usize) -> &Arc<str> {
        &self.paths[file]
    }

    pub fn includes(&self, file: usize) -> &[usize] {
        &self.edges[file]
    }
}

pub struct Context<'a> {
    scope: &'a Scope,
    file: usize,
    line: u32,
    visible: Arc<Vec<u64>>,
}

impl Context<'_> {
    fn sees(&self, file: usize) -> bool {
        file == self.file || self.scope.bit[file].is_some_and(|b| self.visible[b / 64] & (1 << (b % 64)) != 0)
    }
}

impl Lookup for Context<'_> {
    fn is_object_like(&self, name: &str) -> bool {
        !KEYWORDS.contains(&name)
            && self
                .scope
                .object_like
                .get(name)
                .is_some_and(|fs| fs.iter().any(|&f| self.sees(f)))
    }

    fn resolve(&self, name: &str) -> Resolved {
        let none = Resolved {
            defs: Vec::new(),
            uncertain: false,
        };
        if KEYWORDS.contains(&name) {
            return none;
        }
        let Some(all) = self.scope.defs.get(name) else {
            return none;
        };
        let undefs = self.scope.undefs.get(name).map_or(&[][..], Vec::as_slice);
        // A local `#undef` before this line ends every earlier definition,
        // including those from headers.
        let local_undef = undefs
            .iter()
            .filter(|(f, l)| *f == self.file && *l < self.line)
            .map(|(_, l)| *l)
            .max();
        let mut chosen: Vec<&Def> = all
            .iter()
            .filter(|d| {
                if d.file == self.file {
                    d.line < self.line && local_undef.is_none_or(|u| d.line > u)
                } else {
                    local_undef.is_none() && d.exported && self.sees(d.file)
                }
            })
            .collect();
        // A header that `#undef`s this name means which definition applies
        // depends on include order, which is not modelled.
        let uncertain = chosen.iter().any(|d| d.file != self.file)
            && self
                .scope
                .foreign_undefs
                .get(name)
                .is_some_and(|fs| fs.iter().any(|&f| f != self.file && self.sees(f)));
        let mut seen = std::collections::HashSet::new();
        chosen.retain(|d| seen.insert(d.key.clone()));
        Resolved {
            defs: chosen.into_iter().map(|d| d.def.clone()).collect(),
            uncertain,
        }
    }
}

/// Files an include directive can refer to.
fn resolve_include(
    includer: &str,
    inc: &crate::Include,
    by_path: &HashMap<&str, usize>,
    arches: &[&str],
) -> Vec<usize> {
    let find = |p: &str| by_path.get(p).copied();
    if !inc.system {
        let dir = includer.rsplit_once('/').map_or("", |(d, _)| d);
        if let Some(id) = normalize(&format!("{dir}/{}", inc.path)).and_then(|p| find(&p)) {
            return vec![id];
        }
    }
    let mut candidates = Vec::new();
    if let Some(rest) = inc.path.strip_prefix("asm/") {
        for a in arches {
            candidates.push(format!("arch/{a}/include/asm/{rest}"));
            candidates.push(format!("arch/{a}/include/uapi/asm/{rest}"));
        }
        // Architectures without their own header get asm-generic's.
        candidates.push(format!("include/asm-generic/{rest}"));
        candidates.push(format!("include/uapi/asm-generic/{rest}"));
    } else {
        candidates.push(format!("include/{}", inc.path));
        candidates.push(format!("include/uapi/{}", inc.path));
        for a in arches {
            candidates.push(format!("arch/{a}/include/{}", inc.path));
            candidates.push(format!("arch/{a}/include/uapi/{}", inc.path));
        }
    }
    candidates
        .iter()
        .filter_map(|c| normalize(c).and_then(|p| find(&p)))
        .collect()
}

/// Resolves `.` and `..` components.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// Iterative Tarjan: component of each node, and members of each component.
fn tarjan(edges: &[Vec<usize>]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let n = edges.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut comp = vec![usize::MAX; n];
    let mut members = Vec::new();
    let mut next = 0;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work = vec![(root, 0usize)];
        while let Some(&mut (v, ref mut child)) = work.last_mut() {
            if *child == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = edges[v].get(*child) {
                *child += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let id = members.len();
                let mut group = Vec::new();
                loop {
                    let w = stack.pop().expect("v is on the stack");
                    on_stack[w] = false;
                    comp[w] = id;
                    group.push(w);
                    if w == v {
                        break;
                    }
                }
                members.push(group);
            }
        }
    }
    (comp, members)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;

    fn scope(files: &[(&str, &str)]) -> (Scope, Vec<FileFacts>) {
        let facts: Vec<FileFacts> = files.iter().map(|(_, src)| analyze(src.as_bytes())).collect();
        let refs: Vec<(Arc<str>, &FileFacts)> =
            files.iter().zip(&facts).map(|((p, _), f)| (Arc::from(*p), f)).collect();
        (Scope::new(&refs), facts)
    }

    fn bodies(ctx: &Context, name: &str) -> Vec<String> {
        ctx.resolve(name).defs.iter().map(|d| d.body.clone()).collect()
    }

    #[test]
    fn only_included_headers_are_visible() {
        let (s, _) = scope(&[
            ("include/linux/a.h", "#include <linux/b.h>\n#define A(x) a\n"),
            ("include/linux/b.h", "#define B(x) b\n"),
            ("drivers/x/one.c", "#include <linux/a.h>\n#define SHOW(x) one\n"),
            ("drivers/y/two.c", "#define SHOW(x) two\n"),
        ]);
        let ctx = s.at(2, 100);
        assert_eq!(bodies(&ctx, "A"), ["a"]);
        assert_eq!(bodies(&ctx, "B"), ["b"], "transitive include");
        assert_eq!(
            bodies(&ctx, "SHOW"),
            ["one"],
            "another file's local macro is not visible"
        );
    }

    #[test]
    fn asm_includes_union_over_architectures() {
        let (s, _) = scope(&[
            ("arch/x86/include/asm/w.h", "#define W(x) x86\n"),
            ("arch/arm64/include/asm/w.h", "#define W(x) arm64\n"),
            ("include/asm-generic/w.h", "#define W(x) generic\n"),
            ("kernel/k.c", "#include <asm/w.h>\n"),
        ]);
        let mut b = bodies(&s.at(3, 10), "W");
        b.sort();
        assert_eq!(b, ["arm64", "generic", "x86"]);
    }

    #[test]
    fn quoted_includes_are_relative_first() {
        let (s, _) = scope(&[
            ("drivers/x/local.h", "#define L(x) local\n"),
            ("include/local.h", "#define L(x) global\n"),
            ("drivers/x/x.c", "#include \"local.h\"\n"),
        ]);
        assert_eq!(bodies(&s.at(2, 10), "L"), ["local"]);
    }

    #[test]
    fn position_and_local_undef() {
        let src = "#define SHOW(x) first\n\n#undef SHOW\n#define SHOW(x) second\n";
        let (s, _) = scope(&[
            ("include/h.h", "#define SHOW(x) header\n"),
            ("a.c", &format!("#include <h.h>\n{src}")),
        ]);
        assert_eq!(bodies(&s.at(1, 1), "SHOW"), ["header"], "before any local definition");
        let mut b = bodies(&s.at(1, 3), "SHOW");
        b.sort();
        assert_eq!(b, ["first", "header"]);
        assert!(
            bodies(&s.at(1, 5), "SHOW").is_empty(),
            "after #undef, before the new definition"
        );
        assert_eq!(bodies(&s.at(1, 6), "SHOW"), ["second"]);
    }

    #[test]
    fn redefinition_in_a_header_is_uncertain() {
        let (s, _) = scope(&[
            ("include/stage1.h", "#define TRACE_EVENT(x) one\n"),
            ("include/stage2.h", "#undef TRACE_EVENT\n#define TRACE_EVENT(x) two\n"),
            ("t.c", "#include <stage1.h>\n#include <stage2.h>\n"),
        ]);
        assert!(s.at(2, 10).resolve("TRACE_EVENT").uncertain);
    }

    #[test]
    fn header_private_helpers_are_not_visible() {
        // include/linux/poll.h defines __MAP, uses it, and #undefs it.
        let (s, _) = scope(&[
            (
                "include/poll.h",
                "#define __MAP(v, from, to) x\nint a = __MAP(1, 2, 3);\n#undef __MAP\n",
            ),
            ("include/syscalls.h", "#define __MAP(n, ...) __MAP##n(__VA_ARGS__)\n"),
            ("k.c", "#include <poll.h>\n#include <syscalls.h>\n"),
        ]);
        let r = s.at(2, 10).resolve("__MAP");
        assert!(!r.uncertain);
        assert_eq!(
            r.defs.iter().map(|d| d.body.as_str()).collect::<Vec<_>>(),
            ["__MAP##n(__VA_ARGS__)"]
        );
    }

    #[test]
    fn every_ifdef_branch_definition_is_exported() {
        // arch/x86/include/asm/syscall_wrapper.h defines __X64_SYS_STUBx in
        // both branches of #ifdef CONFIG_X86_64.
        let (s, _) = scope(&[
            (
                "include/w.h",
                "#ifdef CONFIG_X86_64\n#define STUB(x) real\n#else\n#define STUB(x)\n#endif\n",
            ),
            ("k.c", "#include <w.h>\n"),
        ]);
        assert_eq!(s.at(1, 10).resolve("STUB").defs.len(), 2);
    }

    #[test]
    fn keywords_are_never_macros() {
        let (s, _) = scope(&[("c.c", "#define if(cond, ...) if (trace(cond))\n")]);
        assert!(s.at(0, 10).resolve("if").defs.is_empty());
    }

    #[test]
    fn identical_definitions_count_once() {
        let (s, _) = scope(&[
            ("arch/x86/include/asm/m.h", "#define M(x) same(x)\n"),
            ("arch/arm/include/asm/m.h", "#define M(x)   same( x )\n"),
            ("k.c", "#include <asm/m.h>\n"),
        ]);
        assert_eq!(s.at(2, 10).resolve("M").defs.len(), 1);
    }

    #[test]
    fn include_cycles() {
        let (s, _) = scope(&[
            ("include/a.h", "#include <b.h>\n#define A(x) a\n"),
            ("include/b.h", "#include <a.h>\n#define B(x) b\n"),
            ("k.c", "#include <a.h>\n"),
        ]);
        let ctx = s.at(2, 10);
        assert_eq!(bodies(&ctx, "A"), ["a"]);
        assert_eq!(bodies(&ctx, "B"), ["b"]);
    }
}
