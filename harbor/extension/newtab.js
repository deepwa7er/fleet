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

  main.append(dot, name, meta);
  li.append(main);

  if (entry.summary) {
    const sum = document.createElement("div");
    sum.className = "row-sum";
    sum.textContent = entry.summary;
    li.append(sum);
  }

  // Live commit activity (truthful recency), when available.
  if (entry.activity) {
    const act = document.createElement("div");
    act.className = "row-act";
    const when = relTime(entry.activity.date);
    act.textContent = `↻ ${when} · ${entry.activity.sha} ${entry.activity.message}`;
    act.title = entry.activity.message;
    li.append(act);
  }

  const open = () => openNote(entry.name, entry.repo);
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

async function openNote(name, repo) {
  document.getElementById("sheet-title").textContent = name;
  const repoLink = document.getElementById("sheet-repo");
  if (repo) {
    repoLink.href = `https://github.com/${repo}`;
    repoLink.classList.remove("hidden");
  } else {
    repoLink.classList.add("hidden");
  }
  const body = document.getElementById("sheet-body");
  body.textContent = "loading…";
  overlay.classList.remove("hidden");

  try {
    const res = await fetch(`${API}/api/note/${encodeURIComponent(name)}`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const note = await res.json();
    body.innerHTML = note.html; // trusted: our own secondbrain notes, rendered server-side
  } catch (e) {
    body.textContent = `couldn't load note (${e.message || e})`;
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
async function load() {
  if (!API) {
    showError("no API configured");
    return;
  }
  try {
    const res = await fetch(`${API}/api/state`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const state = await res.json();
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
  if (m) openNote(decodeURIComponent(m[1]));
}
window.addEventListener("hashchange", openFromHash);

// ── boot ─────────────────────────────────────────────────────────────────
tick();
setInterval(tick, 1000 * 15);
load();
setInterval(load, 1000 * 60);
openFromHash();
