"use strict";

// Pages are rendered by the server (crates/cli/src/pages.rs). This script only
// adds search-as-you-type and tooltips; everything works without it.

const $ = (sel, root = document) => root.querySelector(sel);
const main = $("#main");
const input = $("#q");
const list = $("#suggestions");
const tip = $("#tip");

function esc(s) {
  return String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

const pageOf = name => `/f/${encodeURIComponent(name)}`;

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
  } else if (e.key === "Enter" && active >= 0 && !list.hidden) {
    // A highlighted suggestion; otherwise the form submits to /search.
    e.preventDefault();
    location.assign(pageOf(items[active]));
  } else if (e.key === "Escape") {
    closeList();
  }
});

list.addEventListener("mousedown", e => {
  const li = e.target.closest("li");
  if (!li) return;
  e.preventDefault();
  location.assign(pageOf(items[Number(li.id.slice(1))]));
});
input.addEventListener("blur", () => setTimeout(closeList, 100));
document.addEventListener("keydown", e => {
  if (e.key === "/" && document.activeElement !== input) {
    e.preventDefault();
    input.focus();
  }
});
