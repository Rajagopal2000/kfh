"""Compare the function database for one release against a real kernel build.

usage: recall.py <functions.db> <function-names.txt> <tag>
"""
import collections
import re
import sqlite3
import sys

db_path, map_path, tag = sys.argv[1:4]
db = sqlite3.connect(db_path)
(ord_,) = db.execute("SELECT ord FROM versions WHERE tag = ?", (tag,)).fetchone()

rows = db.execute(
    """SELECT d.name, d.file, g.generated_by, g.origin, g.is_static, g.is_inline, g.kind
       FROM defs d JOIN spans s ON s.def_id = d.id JOIN signatures g ON g.id = s.sig_id
       WHERE s.first_ord <= ?1 AND s.last_ord >= ?1 AND g.kind != 'Macro'""",
    (ord_,),
).fetchall()
direct = {r[0] for r in rows if r[2] is None}
generated = {r[0] for r in rows if r[2] is not None}

# Ground truth: a list of function names, one per line (e.g. the BTF FUNC
# entries of a vmlinux, which are exactly the functions that build contains).
symbols = {line.strip() for line in open(map_path) if line.strip()}

print(f"{tag}: {len(symbols)} functions in the reference kernel")
print(f"database: {len(direct)} directly defined names, {len(generated - direct)} more from macro expansion")
hit_direct = symbols & direct
hit_all = symbols & (direct | generated)
print(f"recall, direct only:        {len(hit_direct) / len(symbols):.2%}")
print(f"recall, with generated:     {len(hit_all) / len(symbols):.2%}  (+{len(hit_all) - len(hit_direct)} symbols)")

missing = sorted(symbols - direct - generated)
prefixes = collections.Counter(re.match(r"_*[a-zA-Z0-9]+", m).group(0) if re.match(r"_*[a-zA-Z0-9]+", m) else m for m in missing)
print("\nmost common prefixes among symbols still missing:")
for p, n in prefixes.most_common(25):
    print(f"  {n:>6} {p}")

# Precision of generated functions: those that should exist on x86-64 in a
# file this kernel builds in (some of its direct functions are present).
built_in_files = {r[1] for r in rows if r[2] is None and r[0] in symbols}
checked, found = 0, 0
absent = collections.defaultdict(list)
for name, file, gen_by, origin, is_static, is_inline, kind in rows:
    if gen_by is None or is_inline or name in direct:
        continue
    if origin and "arch/" in origin and "arch/x86/" not in origin:
        continue
    if file not in built_in_files:
        continue
    checked += 1
    if name in symbols:
        found += 1
    else:
        macro = gen_by.split("(")[0]
        absent[macro].append((name, file, bool(is_static), origin))
print(f"\ngenerated, non-inline, x86-relevant, in built-in files: {checked}; present in the reference kernel: {found} ({found / max(checked, 1):.2%})")
print("absent, grouped by generating macro:")
for macro, items in sorted(absent.items(), key=lambda kv: -len(kv[1]))[:30]:
    statics = sum(1 for i in items if i[2])
    print(f"  {len(items):>5} {macro} ({statics} static)  e.g. {items[0][0]}  [{items[0][1]}]  origin={items[0][3]}")
