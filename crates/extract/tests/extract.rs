use extract::{Kind, extract, normalize_type};

fn sig(src: &str) -> Vec<String> {
    extract(src.as_bytes())
        .iter()
        .filter(|f| f.kind != Kind::Macro)
        .map(|f| {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| match &p.name {
                    Some(n) => format!("{}|{}", p.ty, n),
                    None => p.ty.clone(),
                })
                .collect();
            format!("{} {}({})", f.return_type, f.name, params.join(", "))
        })
        .collect()
}

#[test]
fn plain_definition() {
    assert_eq!(
        sig("ssize_t vfs_read(struct file *file, char __user *buf, size_t count, loff_t *pos) { }"),
        ["ssize_t vfs_read(struct file *|file, char __user *|buf, size_t|count, loff_t *|pos)"]
    );
}

#[test]
fn annotations_become_attributes() {
    let fs = extract(
        b"static int __init __cold foo_init(void) { return 0; }\nasmlinkage __visible void notrace bar(void) { }",
    );
    assert_eq!(fs[0].name, "foo_init");
    assert!(fs[0].is_static);
    assert_eq!(fs[0].attributes, ["__init", "__cold"]);
    assert_eq!(fs[0].return_type, "int");
    assert!(fs[0].params.is_empty());
    assert_eq!(fs[1].attributes, ["asmlinkage", "__visible", "notrace"]);
    assert_eq!(fs[1].return_type, "void");
}

#[test]
fn annotations_with_arguments() {
    let fs = extract(b"void __sched lock_x(spinlock_t *l) __acquires(l) { }\n__printf(2, 3) int logit(int lvl, const char *fmt, ...) { }");
    assert_eq!(fs[0].attributes, ["__sched", "__acquires"]);
    assert_eq!(fs[1].name, "logit");
    assert_eq!(fs[1].params.last().unwrap().ty, "...");
}

#[test]
fn pointer_return_and_function_pointer_param() {
    assert_eq!(
        sig("static __always_inline struct page *get_pg(int (*cb)(void *), unsigned long flags) { }"),
        ["struct page * get_pg(int (*)(void *)|cb, unsigned long|flags)"]
    );
}

#[test]
fn inline_detection() {
    let fs = extract(b"static inline bool IS_ERR(const void *ptr) { }\nstatic __always_inline int f(void) { }");
    assert!(fs.iter().all(|f| f.is_inline && f.is_static));
    assert_eq!(fs[0].name, "IS_ERR");
}

#[test]
fn sparse_annotations_kept_in_types() {
    assert_eq!(
        sig("void __iomem *ioremap(phys_addr_t addr, struct foo __rcu **pp) { }"),
        ["void __iomem * ioremap(phys_addr_t|addr, struct foo __rcu **|pp)"]
    );
}

#[test]
fn syscall_macros() {
    let fs = extract(b"SYSCALL_DEFINE3(read, unsigned int, fd, char __user *, buf, size_t, count)\n{\n\treturn 0;\n}\nCOMPAT_SYSCALL_DEFINE0(getpid)\n{\n}\n");
    assert_eq!(fs.len(), 2);
    assert_eq!(
        (fs[0].name.as_str(), fs[0].kind, fs[0].line),
        ("sys_read", Kind::Syscall, 1)
    );
    assert_eq!(fs[0].params[1].ty, "char __user *");
    assert_eq!(fs[0].params[1].name.as_deref(), Some("buf"));
    assert_eq!(
        (fs[1].name.as_str(), fs[1].kind, fs[1].params.len()),
        ("compat_sys_getpid", Kind::CompatSyscall, 0)
    );
}

#[test]
fn macro_definition_is_not_a_syscall() {
    let fs = extract(b"#define SYSCALL_DEFINE1(name, ...) SYSCALL_DEFINEx(1, _##name, __VA_ARGS__)\n");
    assert!(fs.iter().all(|f| f.kind == Kind::Macro));
}

#[test]
fn function_like_macros() {
    let src = "#define kmem_cache_alloc(...)\t\t\talloc_hooks(kmem_cache_alloc_noprof(__VA_ARGS__))\n\
#define NOT_FUNCTION_LIKE (1 << 3)\n\
/* #define commented_out(x) x */\n\
# define two_lines(a, b) \\\n\tdo_it(a, /* why */ b)\n\
#define named_va(fmt, args...) printk(fmt, ##args)\n\
int after(void)\n{\n#define local_helper(v) (v)\n\treturn 0;\n}\n";
    let fs = extract(src.as_bytes());
    let macros: Vec<_> = fs.iter().filter(|f| f.kind == Kind::Macro).collect();
    let names: Vec<_> = macros.iter().map(|f| (f.name.as_str(), f.line)).collect();
    assert_eq!(names, [("kmem_cache_alloc", 1), ("two_lines", 4), ("named_va", 6)]);
    assert_eq!(
        macros[0].body.as_deref(),
        Some("alloc_hooks(kmem_cache_alloc_noprof(__VA_ARGS__))")
    );
    assert_eq!(macros[0].params[0].ty, "...");
    assert_eq!(macros[1].body.as_deref(), Some("do_it(a, b)"));
    assert_eq!(
        macros[1]
            .params
            .iter()
            .map(|p| p.name.clone().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(
        (macros[2].params[1].ty.as_str(), macros[2].params[1].name.as_deref()),
        ("...", Some("args"))
    );
    assert!(fs.iter().any(|f| f.name == "after" && f.kind == Kind::Regular));
}

#[test]
fn bpf_call_macro() {
    let fs = extract(b"BPF_CALL_2(bpf_map_lookup_elem, struct bpf_map *, map, void *, key)\n{\n}\n");
    assert_eq!(fs[0].name, "bpf_map_lookup_elem");
    assert_eq!(fs[0].kind, Kind::BpfCall);
    assert_eq!(fs[0].return_type, "u64");
}

#[test]
fn definitions_inside_ifdef_and_declarations_ignored() {
    let src = "int proto(int a);\n#ifdef CONFIG_X\nint real(int a) { }\n#else\nstatic inline int real(int a) { return 0; }\n#endif\n";
    let fs = extract(src.as_bytes());
    assert_eq!(fs.len(), 2);
    assert!(fs.iter().all(|f| f.name == "real"));
    assert_eq!(fs[1].line, 5);
}

#[test]
fn type_normalization() {
    assert_eq!(normalize_type("const  char*"), "const char *");
    assert_eq!(normalize_type("struct  foo  * *"), "struct foo **");
    assert_eq!(normalize_type("int ( * ) ( void * )"), "int (*)(void *)");
    assert_eq!(normalize_type("u8 /* flags */ "), "u8");
    assert_eq!(normalize_type("int [ ]"), "int[]");
}

#[test]
fn macro_tricks_in_a_body_do_not_hide_later_functions() {
    // Shape taken from net/core/dev.c netdev_cmd_to_name().
    let src = "const char *netdev_cmd_to_name(enum netdev_cmd cmd)\n{\n#define N(val) \\\n\tcase NETDEV_##val: \\\n\t\treturn \"NETDEV_\" __stringify(val);\n\tswitch (cmd) {\n\tN(UP) N(DOWN)\n\t}\n#undef N\n\treturn \"UNKNOWN\";\n}\n\nstatic int later(struct notifier_block *nb)\n{\n\treturn 0;\n}\n";
    let names: Vec<_> = extract(src.as_bytes()).into_iter().map(|f| (f.name, f.line)).collect();
    assert_eq!(
        names,
        [("netdev_cmd_to_name".to_string(), 1), ("later".to_string(), 13)]
    );
}

#[test]
fn commented_macro_continuation_does_not_hide_later_functions() {
    // Shape taken from net/core/pktgen.c PKT_FLAGS.
    let src = "#define PKT_FLAGS \\\n\tpf(IPV6)\t/* v6 */\t\\\n\tpf(IPSRC_RND)\t/* rnd */\n\nstatic int pgctrl_show(struct seq_file *seq, void *v)\n{\n\treturn 0;\n}\n";
    let names: Vec<_> = extract(src.as_bytes()).into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["pgctrl_show"]);
}

#[test]
fn comment_markers_inside_strings_are_not_comments() {
    let src = "static const char *s = \"/*\";\nint after(void)\n{\n}\n";
    assert_eq!(extract(src.as_bytes())[0].name, "after");
}

#[test]
fn includes_undefs_and_invocations() {
    let src = "#include <linux/fs.h>\n#include \"local.h\"\n#include MACRO_HEADER\n\
#define SHOW(x) static int x##_show(void) { return 0; }\n\
SHOW(foo)\n\
#undef SHOW\n\
static DEFINE_MUTEX(lock);\n\
META_COLLECTOR(int_random)\n{\n\treturn;\n}\n\
static int\nnot_an_invocation(int a)\n{\n}\n\
#define MULTI(a) \\\nIN_DIRECTIVE(a)\n";
    let f = extract::analyze(src.as_bytes());
    assert_eq!(
        f.includes,
        [
            extract::Include {
                path: "linux/fs.h".into(),
                system: true
            },
            extract::Include {
                path: "local.h".into(),
                system: false
            },
        ]
    );
    assert_eq!(f.undefs, [("SHOW".to_string(), 6)]);
    let inv: Vec<_> = f
        .invocations
        .iter()
        .map(|i| (i.name.as_str(), i.line, i.followed_by_brace))
        .collect();
    assert_eq!(
        inv,
        [
            ("SHOW", 5, false),
            ("DEFINE_MUTEX", 7, false),
            ("META_COLLECTOR", 8, true)
        ]
    );
    assert_eq!(f.invocations[1].text, "DEFINE_MUTEX(lock)");
    assert_eq!(
        f.macros.iter().map(|m| (m.name.as_str(), m.line)).collect::<Vec<_>>(),
        [("SHOW", 4), ("MULTI", 16)]
    );
}

#[test]
fn typeof_parameter_types() {
    // Parameter type produced by the kernel's __SC_LONG in SYSCALL_DEFINEx.
    let ty = "__typeof(__builtin_choose_expr((__builtin_types_compatible_p(typeof((unsigned int)0), typeof(0LL)) || __builtin_types_compatible_p(typeof((unsigned int)0), typeof(0ULL))), 0LL, 0L))";
    let src = format!("long SyS_read({ty} fd, {ty} buf)\n{{\n}}\n");
    let fs = extract(src.as_bytes());
    assert_eq!(fs.len(), 1);
    let params: Vec<_> = fs[0].params.iter().map(|p| p.name.as_deref().unwrap()).collect();
    assert_eq!(params, ["fd", "buf"]);
    assert!(fs[0].params[0].ty.starts_with("__typeof(__builtin_choose_expr("));
    assert_eq!(sig("typeof(int) *f(void) { }"), ["typeof(int) * f()"]);
}

#[test]
fn conditions_of_definitions_and_invocations() {
    let src = "#ifndef _GUARD_H\n#define _GUARD_H\n\
#ifdef CONFIG_LTO_CLANG\n#define STUB(fn) lto\n#else\n#define STUB(fn) plain\n#endif\n\
#if BITS_PER_LONG == 32\nSYSCALL_DEFINE2(truncate64, const char __user *, path, loff_t, length)\n{\n}\n#endif\n\
#if defined(CONFIG_A) && IS_ENABLED(CONFIG_B)\nGEN(x)\n#endif\n#endif\n";
    let f = extract::analyze(src.as_bytes());
    let conds: Vec<Vec<(u32, u32, bool)>> = f
        .macros
        .iter()
        .map(|m| {
            m.conditions
                .iter()
                .map(|c| (c.block, c.branch, c.config_only))
                .collect()
        })
        .collect();
    assert_eq!(
        conds,
        [vec![(1, 0, true)], vec![(1, 1, true)]],
        "guard block 0 is not counted"
    );
    type Conds = Vec<(u32, u32, bool)>;
    let inv: Vec<(&str, Conds)> = f
        .invocations
        .iter()
        .map(|i| {
            (
                i.name.as_str(),
                i.conditions
                    .iter()
                    .map(|c| (c.block, c.branch, c.config_only))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        inv,
        [("SYSCALL_DEFINE2", vec![(2, 0, false)]), ("GEN", vec![(3, 0, true)])]
    );
}
