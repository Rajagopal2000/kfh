"""Precision of generated functions against a real kernel's function list.

A file counts as built in when it defines a global function whose name is
unique in that release and present in the reference kernel. Generated
functions checked: global (not static, not inline), and either unconditional
or decided only by x86 / architecture-neutral headers.
"""
import collections
import sqlite3
import sys

db_path, names_path, tag = sys.argv[1:4]
db = sqlite3.connect(db_path)
(o,) = db.execute("SELECT ord FROM versions WHERE tag = ?", (tag,)).fetchone()
rows = db.execute(
    """SELECT d.name, d.file, g.generated_by, g.origin, g.is_static, g.is_inline
       FROM defs d JOIN spans s ON s.def_id = d.id JOIN signatures g ON g.id = s.sig_id
       WHERE s.first_ord <= ?1 AND s.last_ord >= ?1 AND g.kind != 'Macro'""",
    (o,),
).fetchall()
real = {l.strip() for l in open(names_path) if l.strip()}
files_of = collections.defaultdict(set)
for name, file, *_ in rows:
    files_of[name].add(file)
built_in = {
    file
    for name, file, gen, origin, static, inline in rows
    if gen is None and not static and not inline and len(files_of[name]) == 1 and name in real
}
checked, present = 0, 0
absent = []
seen = set()
for name, file, gen, origin, static, inline in rows:
    if gen is None or static or inline or file not in built_in or (name, file) in seen:
        continue
    if origin and (origin.endswith("(architectures without their own definition)")
                   or any(part.startswith("arch/") and not part.startswith("arch/x86/") for part in origin.split(", "))):
        continue
    seen.add((name, file))
    checked += 1
    if name in real:
        present += 1
    else:
        absent.append((gen.split("(")[0], name, file, origin))
print(f"{len(built_in)} built-in files; checked {checked} global generated functions; present {present} ({present / checked:.2%})")
by_macro = collections.defaultdict(list)
for a in absent:
    by_macro[a[0]].append(a)
for macro, items in sorted(by_macro.items(), key=lambda kv: -len(kv[1])):
    print(f"{len(items):>5} {macro}: " + "; ".join(f"{n} [{f}] via={o}" for _, n, f, o in items[:3]))
