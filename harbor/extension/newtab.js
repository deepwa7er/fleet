// harbor new-tab: a live view into the secondbrain, served by harbor-server.

const API = (window.HARBOR && window.HARBOR.api) || "";

// ── clock ────────────────────────────────────────────────────────────────
function tick() {
  const now = new Date();
  document.getElementById("clock").textContent =
    now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  const date = now.toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" });
  const hour = now.getHours();
  const part =
    hour < 5  ? "still dark out" :
    hour < 12 ? "good morning" :
    hour < 17 ? "good afternoon" :
    hour < 21 ? "good evening" :
                "burning the midnight oil";
  document.getElementById("greeting").textContent = `${part} · ${date}`;
}

// ── time helpers ───────────────────────────────────────────────────────────
function relTime(iso) {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const s = Math.max(0, (Date.now() - then) / 1000);
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  const d = Math.floor(s / 86400);
  if (d < 30) return `${d}d ago`;
  if (d < 365) return `${Math.floor(d / 30)}mo ago`;
  return `${Math.floor(d / 365)}y ago`;
}

function ago(unixSeconds) {
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

// ── fleet / areas rendering ──────────────────────────────────────────────────
function entryRow(entry) {
  const li = document.createElement("li");
  li.className = "entry";
  li.tabIndex = 0;
  li.dataset.name = entry.name;
  if (entry.repo) li.dataset.repo = entry.repo;

  const main = document.createElement("div");
  main.className = "row-main";

  const dot = document.createElement("span");
  dot.className = `dot status-${entry.status}`;
  dot.title = entry.status;

  const name = document.createElement("span");
  name.className = "name";
  name.textContent = entry.name;

  const meta = document.createElement("span");
  meta.className = "meta";
  meta.textContent = entry.phase || entry.status;

  main.append(dot, name);
  if (entry.next_items && entry.next_items.length) {
    const chip = document.createElement("span");
    chip.className = "next-chip";
    chip.textContent = `next ${entry.next_items.length}`;
    chip.title = "documented next steps";
    main.append(chip);
  }
  main.append(meta);
  li.append(main);

  if (entry.summary) {
    const sum = document.createElement("div");
    sum.className = "row-sum";
    sum.textContent = entry.summary;
    li.append(sum);
  }

  // Latest commit (truthful recency), when available.
  const latest = entry.recent && entry.recent[0];
  if (latest) {
    const act = document.createElement("div");
    act.className = "row-act";
    act.textContent = `↻ ${relTime(latest.date)} · ${latest.sha} ${latest.message}`;
    act.title = latest.message;
    li.append(act);
  }

  const open = () => openNote(entry);
  li.addEventListener("click", open);
  li.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(); }
  });
  return li;
}

function fill(listId, entries) {
  const ul = document.getElementById(listId);
  ul.removeAttribute("aria-busy");
  ul.replaceChildren();
  if (!entries || entries.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "nothing here yet";
    ul.append(li);
    return;
  }
  for (const entry of entries) ul.append(entryRow(entry));
}

// ── aggregated "Next up" across all projects/areas ───────────────────────────
const STATUS_RANK = { active: 0, ongoing: 1, shipped: 2, stable: 3, learning: 4, idea: 5, archived: 6 };

function buildNextUp(state) {
  const ul = document.getElementById("nextup");
  const sec = document.getElementById("nextup-section");
  ul.replaceChildren();

  const entries = [...(state.projects || []), ...(state.areas || [])];
  const rows = [];
  for (const e of entries) {
    for (const text of e.next_items || []) rows.push({ entry: e, text });
  }
  rows.sort((a, b) =>
    (STATUS_RANK[a.entry.status] ?? 9) - (STATUS_RANK[b.entry.status] ?? 9) ||
    a.entry.name.localeCompare(b.entry.name));

  if (rows.length === 0) { sec.classList.add("hidden"); return; }
  sec.classList.remove("hidden");

  for (const { entry, text } of rows) {
    const li = document.createElement("li");
    li.tabIndex = 0;

    const dot = document.createElement("span");
    dot.className = `dot status-${entry.status}`;
    dot.title = entry.status;

    const txt = document.createElement("span");
    txt.className = "todo-text";
    txt.textContent = text;

    const proj = document.createElement("span");
    proj.className = "todo-proj";
    proj.textContent = entry.name;

    li.append(dot, txt, proj);
    const open = () => openNote(entry);
    li.addEventListener("click", open);
    li.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(); }
    });
    ul.append(li);
  }
}

// ── services rendering ───────────────────────────────────────────────────────
function serviceClass(s) {
  if (s.active_state === "active") return "svc-active";
  if (s.active_state === "failed") return "svc-failed";
  return "svc-inactive";
}

function fillServices(services) {
  const ul = document.getElementById("services");
  ul.replaceChildren();
  if (!services || services.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "—";
    ul.append(li);
    return;
  }
  for (const s of services) {
    const li = document.createElement("li");
    const main = document.createElement("div");
    main.className = "row-main";

    const dot = document.createElement("span");
    dot.className = `dot ${serviceClass(s)}`;
    dot.title = `${s.active_state} / ${s.sub_state}`;

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = s.name || s.unit.replace(/\.service$/, "");

    const meta = document.createElement("span");
    meta.className = "meta";
    meta.textContent = s.sub_state;

    main.append(dot, name, meta);
    li.append(main);
    ul.append(li);
  }
}

// ── note overlay ─────────────────────────────────────────────────────────────
const overlay = document.getElementById("overlay");

function section(title) {
  const h = document.createElement("h4");
  h.className = "sheet-section";
  h.textContent = title;
  return h;
}

async function openNote(entry) {
  const name = entry.name;
  document.getElementById("sheet-title").textContent = name;
  const repoLink = document.getElementById("sheet-repo");
  if (entry.repo) {
    repoLink.href = `https://github.com/${entry.repo}`;
    repoLink.classList.remove("hidden");
  } else {
    repoLink.classList.add("hidden");
  }

  const body = document.getElementById("sheet-body");
  body.replaceChildren();
  overlay.classList.remove("hidden");

  // Next steps (from the note's ## Next section).
  if (entry.next_items && entry.next_items.length) {
    const box = document.createElement("div");
    box.className = "next-box";
    const ul = document.createElement("ul");
    ul.className = "next-list";
    for (const item of entry.next_items) {
      const li = document.createElement("li");
      li.textContent = item;
      ul.append(li);
    }
    box.append(ul);
    body.append(section("Next steps"), box);
  }

  // Recent work (from git).
  if (entry.recent && entry.recent.length) {
    const ul = document.createElement("ul");
    ul.className = "recent";
    for (const c of entry.recent) {
      const li = document.createElement("li");
      const when = document.createElement("span");
      when.className = "rc-when";
      when.textContent = relTime(c.date);
      const msg = document.createElement("span");
      msg.className = "rc-msg";
      msg.textContent = c.message;
      li.append(when, msg);
      ul.append(li);
    }
    body.append(section("Recent work"), ul);
  }

  // Full note.
  const note = document.createElement("div");
  note.className = "markdown";
  note.textContent = "loading note…";
  body.append(section("Note"), note);
  try {
    const res = await fetch(`${API}/api/note/${encodeURIComponent(name)}`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    note.innerHTML = data.html; // trusted: our own secondbrain notes, rendered server-side
  } catch (e) {
    note.textContent = `couldn't load note (${e.message || e})`;
  }
}

function closeNote() {
  overlay.classList.add("hidden");
  document.getElementById("sheet-body").replaceChildren();
}

overlay.addEventListener("click", (e) => { if (e.target === overlay) closeNote(); });
document.getElementById("sheet-close").addEventListener("click", closeNote);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !overlay.classList.contains("hidden")) closeNote();
});

// ── footer status ────────────────────────────────────────────────────────────
function setStatus(text, kind) {
  const el = document.getElementById("status");
  el.textContent = text;
  el.className = kind || "";
}

function showError(message) {
  document.getElementById("nextup-section").classList.add("hidden");
  for (const id of ["fleet", "areas", "services"]) {
    const ul = document.getElementById(id);
    ul.removeAttribute("aria-busy");
    ul.replaceChildren();
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "—";
    ul.append(li);
  }
  setStatus(`can't reach harbor (${message}) — is the server up / are you on the tailnet?`, "err");
}

// ── data ─────────────────────────────────────────────────────────────────
let lastState = null;

async function load() {
  if (!API) {
    showError("no API configured");
    return;
  }
  try {
    const res = await fetch(`${API}/api/state`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const state = await res.json();
    lastState = state;
    buildNextUp(state);
    fill("fleet", state.projects);
    fill("areas", state.areas);
    fillServices(state.services);
    const commit = state.source && state.source.commit ? ` @${state.source.commit}` : "";
    setStatus(`live · secondbrain${commit} · refreshed ${ago(state.generated_at)}`, "ok");
  } catch (e) {
    showError(e.message || String(e));
  }
}

// Deep-link: #note=<name> opens that note on load / hash change.
function openFromHash() {
  const m = location.hash.match(/^#note=(.+)$/);
  if (!m) return;
  const name = decodeURIComponent(m[1]);
  const all = lastState ? [...(lastState.projects || []), ...(lastState.areas || [])] : [];
  openNote(all.find((e) => e.name === name) || { name });
}
window.addEventListener("hashchange", openFromHash);

// ── boot ─────────────────────────────────────────────────────────────────
tick();
setInterval(tick, 1000 * 15);
load().then(openFromHash);
setInterval(load, 1000 * 60);
