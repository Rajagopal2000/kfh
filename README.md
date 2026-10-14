# kfh: kernel function history

A kprobe attaches to a kernel function by name, so it breaks when that function is renamed, changes its parameters, gets inlined, or turns into a macro.
`kfh` records the signature of every function definition in every mainline Linux release since v4.0 and shows how each one changed over time.
It tracks added, removed, retyped and renamed parameters, return types, moves between files, and annotations such as `static`, `inline`, `__init` and `notrace` that affect whether a function can be probed.

It also records functions whose names are assembled by macros, such as `__x64_sys_read` from `SYSCALL_DEFINE3(read, ...)` or `foo_open` from `DEFINE_SHOW_ATTRIBUTE(foo)`.
Each one comes with the invocation that generates it.
When the function exists only under some definitions of a macro, the record names those definitions by `file:line`.

When a function becomes a macro, as `kmem_cache_alloc` did in v6.10, `kfh` keeps tracking the macro and lists the real functions it expands to.
Functions that receive the macro's arguments are shown as the ones to probe instead, and the other functions it calls are listed separately.

## Usage

```sh
# One-time: a bare clone of mainline (about 6.5 GB)
git clone --bare https://github.com/torvalds/linux.git data/linux.git

cargo build --release
./target/release/kfh build                # writes data/functions.db (about 2 hours)
./target/release/kfh show tcp_sendmsg     # text history with Elixir links; --json for JSON
./target/release/kfh serve                # web UI on http://127.0.0.1:7878
```

To pick up a new release, run `git --git-dir=data/linux.git fetch --tags` and then `kfh build` again.
`kfh build` does nothing when the database already has every release, and only one build of a database runs at a time.
Before replacing an existing database it checks the new one: every release must still be present, no shared release may lose more than 1% of its functions, and a few functions every release defines must be there.
If a build fails or is rejected, later runs on the same releases stop at once instead of repeating it; fix the cause and run `kfh build --force`.
`kfh build --since 6.14 --limit 1` builds a single release in a few minutes, which is useful when testing changes.

## Container image

CI publishes `ghcr.io/rajagopal2000/kfh:<commit sha>` for every commit on `main`.
The image holds only the `kfh` binary, runs as a non-root user, and works with a read-only root filesystem.
It does not include the database, which is mounted at `/data/functions.db`:

```sh
docker run --read-only -p 7878:7878 -v "$PWD/data/functions.db:/data/functions.db:ro" ghcr.io/rajagopal2000/kfh:<sha>
```

`/healthz` returns `ok` once the database answers a query, and the server finishes in-flight requests before exiting on `SIGTERM`.

## Layout

- `crates/extract` parses unpreprocessed kernel C with tree-sitter.
  Before parsing, it blanks kernel annotations (recording them as attributes), comments, and function bodies, so that macro tricks inside a body cannot confuse the parser.
  It also collects function-like `#define`s, top-level macro invocations, `#include` and `#undef` directives, and the `#if` branch each of these sits in.
  - `cpp.rs` expands macros following C11 6.10.3, using Prosser's hide-set algorithm.
    It handles `#`, `##`, `__VA_ARGS__`, GNU named variadics, `, ## __VA_ARGS__`, and `__VA_OPT__`.
  - `scope.rs` works out which macro definitions are visible at a given line of a file by resolving `#include`s the way the kernel build does.
    `<asm/...>` resolves to every architecture's header plus `asm-generic`.
- `crates/history` builds the SQLite database and answers queries, including the signature diff and move detection.
  It parses each unique file blob once and stores consecutive releases with the same signature as one span.
  Expansion results are cached across releases and reused only while every macro definition they depend on is unchanged.
- `crates/cli` holds the `kfh` binary and the web UI it serves (`web/`).
- `validation/` checks the database against a real kernel build, as described below.

## Rules for macro expansion

Without a kernel configuration there is no way to know which definition of a macro applies, so the expander takes the union of the possibilities.
These rules keep each result to something a real build could produce:

- One expansion never mixes definitions from different architectures.
- One expansion takes every definition from the same branch of a given `#if` block, starting with the branches the invocation itself sits in.
- A helper macro that a header defines, uses, and then `#undef`s is not visible to files that include the header.
- If a header `#undef`s another header's macro and redefines it, the outcome depends on include order, so nothing is recorded.
  This is why trace-event functions are missing.
- C keywords are never treated as macros.
  The kernel defines `if` as a macro only under `CONFIG_PROFILE_ALL_BRANCHES`.
- A name that `##` builds from a value only the build knows, such as `__COUNTER__`, `__LINE__`, `KBUILD_MODNAME` or an object-like macro, is rejected.
- Under an `#if` that tests anything other than `CONFIG_*` options (for example `BITS_PER_LONG == 32`), results that rely on an architecture's own definitions are dropped.
  The source alone cannot tell whether that architecture and that condition ever occur together.

## Validation

The scripts in `validation/` compare one release of the database with the functions in a real kernel build.
They read the function list from the kernel's BTF type information, which lists every function the build contains after inlining, static or global.

```sh
python3 validation/btf_funcs.py vmlinux | sort -u > funcs.txt   # vmlinux extracted from a distro vmlinuz
python3 validation/recall.py     data/functions.db funcs.txt v6.14
python3 validation/precision.py  data/functions.db funcs.txt v6.14
```

Results against Fedora 42's 6.14.0 kernel, which has 67,602 functions:

- Recall is 89.4%, up from 86.5% before macro expansion was added.
  Most of the missing functions are trace-event machinery (`trace_event_raw_event_*`, `__traceiter_*`, `perf_trace_*` and similar, about 6,000), BPF trampolines, and functions written in assembly.
- The precision check covers 1,245 generated global functions that should exist on x86-64, in files that this kernel builds in.
  Of these, 1,164 are present in the kernel.
  Of the 81 that are not, 77 are `__x64_compat_sys_*` wrappers that need `CONFIG_X86_X32_ABI`, and two more need other options that Fedora leaves off.
  `__ia32_sys_sigaction` exists only in 32-bit x86 builds.
  `__x64_sys_sigaction` does not exist in any build, because only 32-bit x86 selects `CONFIG_OLD_SIGACTION`, and that rule is in Kconfig, not in the C source.

## Known gaps

- Trace-event functions (`TRACE_EVENT` and related macros) are not generated, because those macros are redefined in stages and the result depends on include order.
- `kfh` does not read Kconfig, so it cannot rule out combinations that Kconfig forbids, such as `__x64_sys_sigaction`.
- When architecture-specific results are dropped under a non-`CONFIG_*` condition, some real names are lost.
  On x86-64 this misses 61 of the 1,009 syscall wrappers.
- Symbols created by `__attribute__((alias))` declarations are not recorded, except for the `sys_*` syscall entry points.
- The history is taken from source code only.
  Whether the compiler inlined a function or emitted it with a suffix such as `.isra.0` depends on the build, and `kfh` does not cover that yet.
