"use strict";

const ERA_COLORS = 6;
const DEFS_SHOWN = 6;
const EXAMPLES = ["tcp_sendmsg", "do_sys_openat2", "vfs_read", "tcp_v4_connect", "sys_openat", "kmem_cache_alloc", "__alloc_pages", "wake_up_new_task"];

const TAG_HELP = {
  static: "Visible only inside its file. The compiler may inline it or emit it with a suffix such as .isra.0, so the plain name may not be probe-able.",
  inline: "Marked inline. There is usually no standalone symbol, so a kprobe on this name will likely fail.",
  __always_inline: "Always inlined. No standalone symbol exists to probe.",
  __init: "Freed after boot. It cannot be probed once the system is up.",
  __exit: "Only used on module unload, and discarded when built into the kernel.",
  notrace: "Excluded from ftrace, so fentry and the function tracer cannot attach.",
  noinstr: "Excluded from instrumentation, including kprobes.",
  __weak: "Weak symbol. An architecture may override it with its own definition.",
  syscall: "Defined with SYSCALL_DEFINE. On x86-64 since v4.17, probe the wrapper __x64_sys_<name> rather than sys_<name>.",
  compat_syscall: "Defined with COMPAT_SYSCALL_DEFINE for 32-bit callers on 64-bit kernels.",
  bpf_helper: "Defined with BPF_CALL_n. The helper wrapper takes u64 arguments.",
  macro: "A preprocessor macro, not a function. No symbol with this name exists to probe; probe a function it expands to instead.",
  generated: "Defined by a macro, so this name never appears literally in the source. The line links to the macro invocation.",
};
const WARN_TAGS = new Set(["macro", "inline", "__always_inline", "__init", "__exit", "notrace", "noinstr"]);

const $ = (sel, root = document) => root.querySelector(sel);
const main = $("#main");
const input = $("#q");
const list = $("#suggestions");
const tip = $("#tip");

function esc(s) {
  return String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function elixir(tag, file, line) {
  return `https://elixir.bootlin.com/linux/${encodeURIComponent(tag)}/source/${file.split("/").map(encodeURIComponent).join("/")}#L${line}`;
}

function paramDecl(p) {
  if (!p.name) return p.ty;
  if (!p.ty) return p.name;
  return p.ty.endsWith("*") ? p.ty + p.name : `${p.ty} ${p.name}`;
}

/* ---------- routing ---------- */

function go(path, replace = false) {
  if (replace) history.replaceState(null, "", path);
  else history.pushState(null, "", path);
  route();
}

function route() {
  const m = location.pathname.match(/^\/f\/(.+)$/);
  if (m) {
    const name = decodeURIComponent(m[1]);
    input.value = name;
    showFunction(name);
  } else {
    input.value = "";
    showLanding();
  }
}

window.addEventListener("popstate", route);
document.addEventListener("click", e => {
  const a = e.target.closest("a[data-route]");
  if (a && !e.metaKey && !e.ctrlKey && !e.shiftKey && e.button === 0) {
    e.preventDefault();
    go(a.getAttribute("href"));
  }
});

/* ---------- landing ---------- */

function showLanding() {
  document.title = "Kernel function history";
  main.innerHTML = `
    <section class="intro">
      <h1>How has this kernel function changed?</h1>
      <p>Search a function to see its signature in every mainline release since v4.0:
      when parameters were added, removed or retyped, and when it moved, became inline or disappeared.
      Each release links to the source on Elixir.</p>
      <ul class="examples">
        ${EXAMPLES.map(n => `<li><a href="/f/${encodeURIComponent(n)}" data-route>${esc(n)}</a></li>`).join("")}
      </ul>
    </section>`;
}

/* ---------- function page ---------- */

let loadSeq = 0;

async function showFunction(name) {
  const seq = ++loadSeq;
  document.title = `${name} - Kernel function history`;
  main.innerHTML = `<p class="notice">Loading ${esc(name)}…</p>`;
  let data;
  try {
    const res = await fetch(`/api/function/${encodeURIComponent(name)}`);
    if (!res.ok) throw new Error(await res.text());
    data = await res.json();
  } catch (err) {
    if (seq === loadSeq) main.innerHTML = `<p class="notice">Could not load ${esc(name)}: ${esc(err.message)}</p>`;
    return;
  }
  if (seq !== loadSeq) return;
  if (data.defs.length === 0) return showNotFound(name, data.versions);
  render(data);
}

async function showNotFound(name, versions) {
  const range = `${versions[0]} to ${versions[versions.length - 1]}`;
  main.innerHTML = `
    <h1 class="fn-name">${esc(name)}</h1>
    <p class="summary">No function with this name is defined in ${esc(range)}. Check the spelling.</p>`;
  // Shorten the name until some functions share its prefix.
  for (let len = name.length - 1; len >= 3; len--) {
    const names = await (await fetch(`/api/search?q=${encodeURIComponent(name.slice(0, len))}`)).json();
    if (!names.length) continue;
    $(".summary").textContent = `No function with this name is defined in ${range}. Did you mean one of these?`;
    main.insertAdjacentHTML("beforeend",
      `<ul class="examples">${names.map(n => `<li><a href="/f/${encodeURIComponent(n)}" data-route>${esc(n)}</a></li>`).join("")}</ul>`);
    return;
  }
}

function render({ name, versions, defs }) {
  const latest = versions.length - 1;
  defs.sort((a, b) => lastOrd(b) - lastOrd(a) || presence(b) - presence(a) || a.file.localeCompare(b.file));

  const isMacro = s => s.signature.kind === "Macro";
  const live = defs.filter(d => lastOrd(d) === latest).length;
  // Definitions in the latest release that are real functions rather than macros.
  const liveFunctions = defs.filter(d => d.spans.some(s => s.last_ord === latest && !isMacro(s))).length;
  const changes = defs.reduce((n, d) => n + d.spans.filter(s => s.changes.length).length, 0);
  const files = defs.length === 1 ? "1 file" : `${defs.length} files`;
  let summary = `Defined in ${files}. `;
  if (live === 0) {
    const last = Math.max(...defs.map(lastOrd));
    summary += `Not present in ${versions[latest]}; last seen in ${versions[last]}. `;
  } else if (liveFunctions === 0) {
    const lastFn = Math.max(-1, ...defs.flatMap(d => d.spans).filter(s => !isMacro(s)).map(s => s.last_ord));
    summary += `In ${versions[latest]} it is a macro, not a function`;
    summary += lastFn >= 0 ? `; it was last a function in ${versions[lastFn]}. ` : ". ";
  } else if (live < defs.length) {
    summary += `Present in ${versions[latest]} in ${live === 1 ? "1 file" : `${live} files`}. `;
  } else {
    summary += `Present in ${versions[latest]}. `;
  }
  const firstOrd = Math.min(...defs.flatMap(d => d.spans).map(s => s.first_ord));
  const since = firstOrd === 0 ? versions[0] : `it first appeared in ${versions[firstOrd]}`;
  summary += changes === 0
    ? `No signature changes since ${since}.`
    : `The signature changed ${changes === 1 ? "once" : `${changes} times`} since ${since}.`;

  main.innerHTML = `
    <h1 class="fn-name">${esc(name)}</h1>
    <p class="summary">${esc(summary)}</p>
    <div id="defs"></div>`;
  // One colour per distinct signature across the whole page, in order of first
  // appearance, so a signature keeps its colour when the function moves files.
  const eraOf = new Map();
  for (const s of defs.flatMap(d => d.spans).sort((a, b) => a.first_ord - b.first_ord)) {
    const key = JSON.stringify(s.signature);
    if (!eraOf.has(key)) eraOf.set(key, eraOf.size % ERA_COLORS);
    s.era = eraOf.get(key);
  }
  const container = $("#defs");
  defs.slice(0, DEFS_SHOWN).forEach(d => container.insertAdjacentHTML("beforeend", renderDef(name, d, versions, defs)));
  if (defs.length > DEFS_SHOWN) {
    const rest = defs.length - DEFS_SHOWN;
    container.insertAdjacentHTML("beforeend", `<button class="more" type="button">Show ${rest} more ${rest === 1 ? "file" : "files"}</button>`);
    $(".more", container).addEventListener("click", e => {
      e.target.remove();
      defs.slice(DEFS_SHOWN).forEach(d => container.insertAdjacentHTML("beforeend", renderDef(name, d, versions, defs)));
    });
  }
}

const lastOrd = d => Math.max(...d.spans.map(s => s.last_ord));
const presence = d => d.spans.reduce((n, s) => n + s.last_ord - s.first_ord + 1, 0);

function renderDef(name, def, versions, defs) {
  const last = def.spans.reduce((a, b) => (b.last_ord > a.last_ord ? b : a));
  const lastLine = last.lines[last.lines.length - 1];
  return `
    <section class="def">
      <h2 class="def-file"><a href="${elixir(last.last, def.file, lastLine)}" target="_blank" rel="noopener">${esc(def.file)}</a></h2>
      ${renderTimeline(def, versions)}
      <ol class="eras">
        ${[...def.spans].sort((a, b) => b.last_ord - a.last_ord || b.first_ord - a.first_ord).map(s => renderEra(name, def, s, versions, defs)).join("")}
      </ol>
    </section>`;
}

function renderTimeline(def, versions) {
  const n = versions.length;
  let cells = "";
  let prevEra = null;
  for (let ord = 0; ord < n; ord++) {
    // When #ifdef variants overlap, show the one that started most recently.
    const span = def.spans.filter(s => s.first_ord <= ord && ord <= s.last_ord).sort((a, b) => b.first_ord - a.first_ord)[0];
    if (!span) {
      cells += `<span class="cell" data-tip="${esc(`${versions[ord]}: not defined in this file`)}"></span>`;
      prevEra = null;
      continue;
    }
    const line = span.lines[ord - span.first_ord];
    const boundary = prevEra !== null && prevEra !== span.era ? " boundary" : "";
    const macro = span.signature.kind === "Macro";
    const tipText = `${versions[ord]}, line ${line}${macro ? ", macro" : ""}`;
    cells += `<a class="cell present${boundary}${macro ? " macro" : ""}" style="--era: var(--era-${span.era})"
      href="${elixir(versions[ord], def.file, line)}" target="_blank" rel="noopener"
      aria-label="${esc(tipText)}" data-tip="${esc(tipText)}"></a>`;
    prevEra = span.era;
  }
  const majors = versions
    .map((v, i) => [v, i])
    .filter(([v], i) => i === 0 || v.split(".")[0] !== versions[i - 1].split(".")[0]);
  // Labels near the right edge hang left of their tick so they stay inside the track.
  const ruler = majors.map(([v, i]) => `<span class="${i / n > 0.9 ? "end" : ""}" style="left: ${(100 * i) / n}%">${esc(v)}</span>`).join("");
  return `<div class="timeline"><div class="track" style="--n: ${n}">${cells}</div><div class="ruler" aria-hidden="true">${ruler}</div></div>`;
}

function renderEra(name, def, span, versions, defs) {
  const sig = span.signature;
  const added = new Set(span.changes.filter(c => c.type === "param_added").map(c => c.param.name ?? c.index));
  const retyped = new Set(span.changes.filter(c => c.type === "param_type_changed" || c.type === "param_renamed").map(c => c.name ?? c.to));
  const retChanged = span.changes.some(c => c.type === "return_type");

  const params = sig.params.length === 0 ? "void" : sig.params.map((p, i) => {
    const cls = added.has(p.name ?? i) ? "p-added" : retyped.has(p.name) ? "p-changed" : "";
    return cls ? `<span class="${cls}">${esc(paramDecl(p))}</span>` : esc(paramDecl(p));
  }).join(", ");
  const macro = sig.kind === "Macro";
  let sigHtml;
  if (macro) {
    // Link the functions the macro calls, since those are what can be probed.
    const callees = new Set(span.targets.flatMap(t => [...t.primary, ...t.also]));
    const body = (sig.body ?? "").split(/([A-Za-z_][A-Za-z0-9_]*)/).map(part =>
      callees.has(part) ? `<a href="/f/${encodeURIComponent(part)}" data-route>${esc(part)}</a>` : esc(part)).join("");
    sigHtml = `#define ${esc(name)}(${sig.params.map(p => esc(macroParam(p))).join(", ")}) ${body}`;
  } else {
    const ret = sig.return_type.endsWith("*") ? sig.return_type : `${sig.return_type} `;
    sigHtml = `${retChanged ? `<span class="p-changed">${esc(ret)}</span>` : esc(ret)}${esc(name)}(${params})`;
  }

  const count = span.last_ord - span.first_ord + 1;
  const range = span.first === span.last ? span.first : `${span.first} to ${span.last}`;

  let history = "";
  if (span.moved_from || span.changes.length) {
    const kindChange = span.changes.find(c => c.type === "kind_changed" && (c.to === "Macro" || c.from === "Macro"));
    let moved = "";
    let shown = span.changes;
    if (span.moved_from && kindChange) {
      // One line says it all: "the function in X was replaced by this macro".
      const what = kindChange.to === "Macro" ? ["function", "macro"] : ["macro", "function"];
      moved = `<li class="changed">In ${esc(span.first)}, the ${what[0]} in <code>${esc(span.moved_from)}</code> was replaced by this ${what[1]}</li>`;
      shown = span.changes.filter(c => c !== kindChange);
    } else if (span.moved_from) {
      moved = `<li class="changed">In ${esc(span.first)}, moved here from <code>${esc(span.moved_from)}</code></li>`;
    }
    history = `<ul class="changes">${moved}${shown.map(c => `<li class="${changeClass(c)}">${describe(c, span.first)}</li>`).join("")}</ul>`;
  } else if (span.first_ord === 0) {
    history = `<p class="first-seen">Already present in ${esc(span.first)}, the oldest release covered.</p>`;
  } else {
    const prevAdjacent = def.spans.some(s => s.last_ord + 1 === span.first_ord);
    history = `<p class="first-seen">${prevAdjacent ? "Variant" : "Appears"} in this file from ${esc(span.first)}.</p>`;
  }

  // Nothing with this name follows in this file: say when it went away.
  const succeeded = def.spans.some(s => s !== span && s.first_ord > span.first_ord && s.first_ord <= span.last_ord + 1);
  let gone = "";
  if (span.last_ord < versions.length - 1 && !succeeded) {
    const next = versions[span.last_ord + 1];
    const movedTo = defs.find(d => d.spans.some(s => s.moved_from === def.file && s.first_ord === span.last_ord + 1));
    const successor = movedTo?.spans.find(s => s.first_ord === span.last_ord + 1);
    if (successor && isMacroSig(successor.signature) !== macro) {
      gone = `<p class="gone">In ${esc(next)}, replaced by a ${macro ? "function" : "macro"} in <code>${esc(movedTo.file)}</code>.</p>`;
    } else if (movedTo) {
      gone = `<p class="first-seen">In ${esc(next)}, moved to <code>${esc(movedTo.file)}</code>.</p>`;
    } else {
      gone = `<p class="gone">Not defined in this file from ${esc(next)} on.</p>`;
    }
  }

  const tags = [];
  const kindTag = { Syscall: "syscall", CompatSyscall: "compat_syscall", BpfCall: "bpf_helper", Macro: "macro" }[sig.kind];
  if (sig.generated_by) tags.push("generated");
  if (kindTag) tags.push(kindTag);
  if (sig.is_static) tags.push("static");
  if (sig.is_inline && !sig.attributes.includes("__always_inline")) tags.push("inline");
  tags.push(...sig.attributes);
  const tagHtml = tags.length
    ? `<div class="tags">${tags.map(t => {
        const help = TAG_HELP[t]?.replaceAll("<name>", name.replace(/^(compat_)?sys_/, ""));
        const attrs = help ? ` tabindex="0" data-tip="${esc(help)}"` : "";
        return `<span class="tag${WARN_TAGS.has(t) ? " warn" : ""}"${attrs}>${esc(t)}</span>`;
      }).join("")}</div>`
    : "";

  const probe = macro ? renderTargets(span) : "";

  const generated = sig.generated_by
    ? `<p class="generated">Generated by <code>${esc(sig.generated_by)}</code>${span.origin ?? sig.origin ? ` via ${originHtml(span.origin ?? sig.origin, span.first)}` : ""}</p>`
    : "";

  const firstLink = elixir(span.first, def.file, span.lines[0]);
  const lastLink = elixir(span.last, def.file, span.lines[span.lines.length - 1]);
  const links = span.first === span.last
    ? `Source: <a href="${firstLink}" target="_blank" rel="noopener">${esc(span.first)}</a>`
    : `Source: <a href="${firstLink}" target="_blank" rel="noopener">${esc(span.first)}</a>, <a href="${lastLink}" target="_blank" rel="noopener">${esc(span.last)}</a>`;

  return `
    <li class="era${macro ? " macro" : ""}" style="--era: var(--era-${span.era})">
      <span class="swatch" aria-hidden="true"></span>
      <div class="range">${esc(range)}<small>${count === 1 ? "1 release" : `${count} releases`}</small></div>
      <div>
        <p class="sig">${sigHtml}</p>
        ${generated}
        ${history}
        ${gone}
        ${tagHtml}
        ${probe}
        <div class="links">${links}</div>
      </div>
    </li>`;
}

const isMacroSig = sig => sig.kind === "Macro";

/* An origin lists macro definitions as `file:line`; link each to its source. */
function originHtml(origin, version) {
  return esc(origin).replace(/([\w.+\/-]+\.[ch]):(\d+)/g, (_, file, line) =>
    `<a href="${elixir(version, file, Number(line))}" target="_blank" rel="noopener"><code>${file}:${line}</code></a>`);
}

const fnLinks = names => names.map(n => `<a href="/f/${encodeURIComponent(n)}" data-route><code>${esc(n)}</code></a>`).join(", ");

/* What a macro calls, per range of releases. Only ranges where the expansion
   could be trusted have entries. */
function renderTargets(span) {
  const ranges = span.targets;
  if (!ranges.length) return "";
  const whole = ranges.length === 1 && ranges[0].first_ord <= span.first_ord && ranges[0].last_ord >= span.last_ord;
  return ranges.map(t => {
    const when = whole ? "" : `<span class="when">${esc(t.first === t.last ? t.first : `${t.first} to ${t.last}`)}</span>`;
    const primary = t.primary.length
      ? `<p class="probe">${when}Probe instead: ${fnLinks(t.primary)}</p>`
      : `<p class="probe">${when}No function receives this macro's arguments.</p>`;
    const also = t.also.length ? `<p class="also">Also calls ${fnLinks(t.also)}</p>` : "";
    return primary + also;
  }).join("");
}

function macroParam(p) {
  if (p.ty === "...") return p.name ? `${p.name}...` : "...";
  return p.name ?? p.ty;
}

function changeClass(c) {
  if (c.type === "param_added" || c.type === "attribute_added") return "added";
  if (c.type === "param_removed" || c.type === "attribute_removed") return "removed";
  return "changed";
}

function describe(c, version) {
  const code = s => `<code>${esc(s)}</code>`;
  switch (c.type) {
    case "param_added": return `In ${esc(version)}, added ${code(paramDecl(c.param))} as parameter ${c.index + 1}`;
    case "param_removed": return `In ${esc(version)}, removed ${code(paramDecl(c.param))}, which was parameter ${c.index + 1}`;
    case "param_type_changed": return `In ${esc(version)}, ${code(c.name)} changed type from ${code(c.from)} to ${code(c.to)}`;
    case "param_renamed": return `In ${esc(version)}, parameter ${c.index + 1} was renamed from ${code(c.from)} to ${code(c.to)}`;
    case "params_reordered": return `In ${esc(version)}, the parameters were reordered`;
    case "return_type": return `In ${esc(version)}, the return type changed from ${code(c.from)} to ${code(c.to)}`;
    case "static_changed": return `In ${esc(version)}, ${c.to ? "it became static" : "it stopped being static"}`;
    case "inline_changed": return `In ${esc(version)}, ${c.to ? "it became inline" : "it stopped being inline"}`;
    case "attribute_added": return `In ${esc(version)}, it gained ${code(c.name)}`;
    case "attribute_removed": return `In ${esc(version)}, it lost ${code(c.name)}`;
    case "kind_changed":
      if (c.to === "Macro") return `In ${esc(version)}, it became a macro`;
      if (c.from === "Macro") return `In ${esc(version)}, it became a function`;
      return `In ${esc(version)}, it changed from ${esc(c.from)} to ${esc(c.to)}`;
    case "macro_body_changed": return `In ${esc(version)}, the expansion changed from ${code(c.from)}`;
    case "generated_by_changed": return c.to
      ? `In ${esc(version)}, generated by ${code(c.to)}${c.from ? ` instead of ${code(c.from)}` : ""}`
      : `In ${esc(version)}, no longer generated by ${code(c.from)}`;
    case "origin_changed": return c.to
      ? `In ${esc(version)}, now comes from ${code(c.to)}`
      : `In ${esc(version)}, no longer depends on ${code(c.from)}`;
    default: return esc(c.type);
  }
}

/* ---------- tooltips ---------- */

function showTip(el, x, y) {
  tip.textContent = el.dataset.tip;
  tip.hidden = false;
  const r = tip.getBoundingClientRect();
  const left = Math.min(Math.max(8, x - r.width / 2), window.innerWidth - r.width - 8);
  const top = y - r.height - 10 < 8 ? y + 18 : y - r.height - 10;
  tip.style.left = `${left}px`;
  tip.style.top = `${top}px`;
}
main.addEventListener("mousemove", e => {
  const el = e.target.closest("[data-tip]");
  if (el) showTip(el, e.clientX, e.clientY);
  else tip.hidden = true;
});
main.addEventListener("mouseleave", () => { tip.hidden = true; });
main.addEventListener("focusin", e => {
  const el = e.target.closest("[data-tip]");
  if (!el) return;
  const r = el.getBoundingClientRect();
  showTip(el, r.left + r.width / 2, r.top);
});
main.addEventListener("focusout", () => { tip.hidden = true; });

/* ---------- search ---------- */

let active = -1;
let items = [];
let searchSeq = 0;
let timer = 0;

function closeList() {
  list.hidden = true;
  input.setAttribute("aria-expanded", "false");
  input.removeAttribute("aria-activedescendant");
  active = -1;
}

function paint() {
  const q = input.value.trim();
  list.innerHTML = items.map((n, i) =>
    `<li id="s${i}" role="option" aria-selected="${i === active}"><b>${esc(n.slice(0, q.length))}</b>${esc(n.slice(q.length))}</li>`).join("");
  if (active >= 0) {
    input.setAttribute("aria-activedescendant", `s${active}`);
    $(`#s${active}`).scrollIntoView({ block: "nearest" });
  }
}

input.addEventListener("input", () => {
  clearTimeout(timer);
  const q = input.value.trim();
  if (!q) { items = []; closeList(); return; }
  timer = setTimeout(async () => {
    const seq = ++searchSeq;
    const res = await fetch(`/api/search?q=${encodeURIComponent(q)}`);
    const names = await res.json();
    if (seq !== searchSeq) return;
    items = names;
    active = -1;
    if (!items.length) { closeList(); return; }
    paint();
    list.hidden = false;
    input.setAttribute("aria-expanded", "true");
  }, 120);
});

input.addEventListener("keydown", e => {
  if (e.key === "ArrowDown" && items.length) {
    e.preventDefault();
    active = (active + 1) % items.length;
    list.hidden = false;
    paint();
  } else if (e.key === "ArrowUp" && items.length) {
    e.preventDefault();
    active = active <= 0 ? items.length - 1 : active - 1;
    paint();
  } else if (e.key === "Enter") {
    const name = active >= 0 && !list.hidden ? items[active] : input.value.trim();
    if (name) { closeList(); input.blur(); go(`/f/${encodeURIComponent(name)}`); }
  } else if (e.key === "Escape") {
    closeList();
  }
});

list.addEventListener("mousedown", e => {
  const li = e.target.closest("li");
  if (!li) return;
  e.preventDefault();
  const name = items[Number(li.id.slice(1))];
  closeList();
  input.blur();
  go(`/f/${encodeURIComponent(name)}`);
});
input.addEventListener("blur", () => setTimeout(closeList, 100));
document.addEventListener("keydown", e => {
  if (e.key === "/" && document.activeElement !== input) {
    e.preventDefault();
    input.focus();
  }
});

route();
