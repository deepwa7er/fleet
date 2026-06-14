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

// ── rendering ──────────────────────────────────────────────────────────────
function entryRow(entry) {
  const li = document.createElement("li");

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

  const sum = document.createElement("div");
  sum.className = "row-sum";
  sum.textContent = entry.summary || "";

  li.append(main, sum);
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

function setStatus(text, kind) {
  const el = document.getElementById("status");
  el.textContent = text;
  el.className = kind || "";
}

function ago(unixSeconds) {
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

function showError(message) {
  for (const id of ["fleet", "areas"]) {
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
    const commit = state.source && state.source.commit ? ` @${state.source.commit}` : "";
    setStatus(`live · secondbrain${commit} · refreshed ${ago(state.generated_at)}`, "ok");
  } catch (e) {
    showError(e.message || String(e));
  }
}

// The command box is inert until wired to ferry; just stop it reloading the tab.
document.getElementById("omni").addEventListener("submit", (e) => e.preventDefault());

// ── boot ─────────────────────────────────────────────────────────────────
tick();
setInterval(tick, 1000 * 15);
load();
setInterval(load, 1000 * 60);
