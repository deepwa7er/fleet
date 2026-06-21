// harbor new-tab: activity pulse across the fleet + quick capture to lagoon.

const API      = (window.HARBOR && window.HARBOR.api)      || "";
const LAGOON_URL = (window.HARBOR && window.HARBOR.lagoon_url) || "";

// ── clock ─────────────────────────────────────────────────────────────────
function tick() {
  document.getElementById("clock").textContent =
    new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// ── time helpers ──────────────────────────────────────────────────────────
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

// ── focus bar — top next step across all projects ─────────────────────────
const STATUS_RANK = { active: 0, ongoing: 1, shipped: 2, stable: 3, learning: 4, idea: 5, archived: 6 };

function buildFocus(state) {
  const bar    = document.getElementById("focus-bar");
  const textEl = document.getElementById("focus-text");
  const projEl = document.getElementById("focus-proj");

  const top = [...(state.projects || []), ...(state.areas || [])]
    .filter((e) => e.next_items && e.next_items.length > 0)
    .sort((a, b) => (STATUS_RANK[a.status] ?? 9) - (STATUS_RANK[b.status] ?? 9))[0];

  if (!top) { bar.classList.add("hidden"); return; }

  textEl.textContent = top.next_items[0];
  projEl.textContent = top.name;
  bar.classList.remove("hidden");

  bar.onclick = () => openNote(top);
  bar.onkeydown = (e) => {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); openNote(top); }
  };
}

// ── activity feed — unified commit timeline across the fleet ──────────────
const COLD_DAYS = 14;

function buildActivity(state) {
  const ul = document.getElementById("activity-feed");
  ul.removeAttribute("aria-busy");
  ul.replaceChildren();

  // Flatten all project commits into a single timeline.
  const events = [];
  for (const p of state.projects || []) {
    for (const c of p.recent || []) {
      events.push({ project: p.name, repo: p.repo, sha: c.sha, date: c.date, message: c.message });
    }
  }
  events.sort((a, b) => Date.parse(b.date) - Date.parse(a.date));

  const recent = events.slice(0, 15);
  if (recent.length === 0) {
    const li = document.createElement("li");
    li.className = "act-empty";
    li.textContent = "no recent activity";
    ul.append(li);
  } else {
    for (const ev of recent) {
    const url = ev.repo ? `https://github.com/${ev.repo}/commit/${ev.sha}` : null;
    ul.append(actRow(ev.project, ev.message, ev.date, false, url));
  }
  }

  // Going-cold: active projects with no commit in COLD_DAYS days.
  const now = Date.now();
  const cold = (state.projects || []).filter((p) => {
    if (p.status !== "active") return false;
    const latest = p.recent?.[0];
    if (!latest) return true;
    return (now - Date.parse(latest.date)) / 86400000 > COLD_DAYS;
  });

  if (cold.length > 0) {
    const divider = document.createElement("li");
    divider.className = "act-divider";
    ul.append(divider);
    for (const p of cold) {
      const latest = p.recent?.[0];
      const label = latest ? `going cold · last active ${relTime(latest.date)}` : "no commit activity";
      ul.append(actRow(p.name, label, null, true));
    }
  }
}

function actRow(project, message, date, cold, url) {
  const li = document.createElement("li");
  li.className = [
    "act-row",
    cold  ? "act-cold"   : "",
    url   ? "act-linked" : "",
  ].filter(Boolean).join(" ");

  if (url) {
    li.tabIndex = 0;
    li.setAttribute("role", "link");
    const open = () => window.open(url, "_blank", "noreferrer");
    li.addEventListener("click", open);
    li.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(); }
    });
  }

  const proj = document.createElement("span");
  proj.className = "act-proj";
  proj.textContent = project;

  const msg = document.createElement("span");
  msg.className = "act-msg";
  msg.textContent = message;

  li.append(proj, msg);

  if (date) {
    const when = document.createElement("span");
    when.className = "act-when";
    when.textContent = relTime(date);
    li.append(when);
  }

  return li;
}

// ── lagoon capture ──────────────────────────────────────────────────────────
function initCapture() {
  const input    = document.getElementById("capture-input");
  const btn      = document.getElementById("capture-btn");
  const statusEl = document.getElementById("capture-status");

  if (!LAGOON_URL) {
    input.disabled = true;
    input.placeholder = "lagoon_url not configured in config.js";
    btn.disabled = true;
    return;
  }

  input.focus();

  async function capture() {
    const text = input.value.trim();
    if (!text) return;

    btn.disabled = true;
    input.disabled = true;
    statusEl.textContent = "capturing…";
    statusEl.className = "capture-status";

    try {
      const res = await fetch(`${LAGOON_URL}/api/thoughts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text }),
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      input.value = "";
      statusEl.textContent = "captured";
      statusEl.className = "capture-status ok";
      setTimeout(() => {
        statusEl.textContent = "";
        statusEl.className = "capture-status";
      }, 2000);
    } catch (e) {
      statusEl.textContent = `failed · ${e.message}`;
      statusEl.className = "capture-status err";
    } finally {
      btn.disabled = false;
      input.disabled = false;
      input.focus();
    }
  }

  btn.addEventListener("click", capture);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      capture();
    }
  });
}

// ── graph view ────────────────────────────────────────────────────────────
const SVGNS = "http://www.w3.org/2000/svg";

function svgEl(tag, attrs) {
  const el = document.createElementNS(SVGNS, tag);
  for (const k in attrs) el.setAttribute(k, attrs[k]);
  return el;
}

const graph = (() => {
  const svg      = document.getElementById("graph-svg");
  const frame    = svg.parentElement;
  const emptyMsg = document.getElementById("graph-empty");

  let nodes    = new Map();
  let edges    = [];
  let services = [];
  let sig      = null;
  let raf      = null;
  let temp     = 0;
  let W = 0, H = 0;
  let visible  = false;
  let drag     = null;

  function runsHealth(units) {
    let known = false, failed = false, allActive = true;
    for (const u of units) {
      if (!u) { allActive = false; continue; }
      const s = services.find((x) => x.unit === u);
      if (!s) { allActive = false; continue; }
      known = true;
      if (s.active_state === "failed") failed = true;
      if (s.active_state !== "active") allActive = false;
    }
    if (!known) return "";
    if (failed) return "h-failed";
    return allActive ? "h-active" : "";
  }

  function unitTitle(units) {
    return units
      .map((u) => {
        const s = services.find((x) => x.unit === u);
        return s ? `${u}: ${s.active_state}/${s.sub_state}` : `${u}: unknown`;
      })
      .join("\n");
  }

  function model(state) {
    const topo = state.topology;
    if (!topo) return null;

    const wantNodes  = new Map();
    const areaByName = new Map((state.areas || []).map((a) => [a.name, a]));
    for (const m of topo.machines || []) {
      wantNodes.set(`m:${m.id}`, {
        id: `m:${m.id}`, type: "machine", label: m.label,
        role: m.role || "", entry: areaByName.get(m.id) || null,
      });
    }
    for (const p of state.projects || []) {
      wantNodes.set(`p:${p.name}`, {
        id: `p:${p.name}`, type: "project", label: p.name, status: p.status, entry: p,
      });
    }

    const wantEdges = [];
    const has = (id) => wantNodes.has(id);
    for (const [name, place] of Object.entries(topo.placements || {})) {
      const from = `p:${name}`;
      if (!has(from)) continue;
      if (place.codebase && has(`m:${place.codebase}`)) {
        wantEdges.push({ from, to: `m:${place.codebase}`, cls: "e-codebase", units: [] });
      }
      const byMachine = new Map();
      for (const r of place.runs_on || []) {
        if (!has(`m:${r.machine}`)) continue;
        const list = byMachine.get(r.machine) || [];
        if (r.unit) list.push(r.unit);
        byMachine.set(r.machine, list);
      }
      for (const [machine, units] of byMachine) {
        wantEdges.push({ from, to: `m:${machine}`, cls: "e-runs", units });
      }
    }
    for (const e of topo.edges || []) {
      const from = `p:${e.from}`, to = `p:${e.to}`;
      if (has(from) && has(to)) wantEdges.push({ from, to, cls: "e-link", label: e.kind, units: [] });
    }

    const signature = JSON.stringify({
      n: [...wantNodes.keys()].sort(),
      e: wantEdges.map((e) => `${e.from}>${e.to}:${e.cls}:${e.label || ""}`).sort(),
    });
    return { wantNodes, wantEdges, signature };
  }

  function rebuild(wantNodes, wantEdges) {
    const next = new Map();
    for (const [id, n] of wantNodes) {
      const prev = nodes.get(id);
      const node = prev ? Object.assign(prev, { entry: n.entry, status: n.status, role: n.role }) : n;
      if (!prev) { node.x = 0; node.y = 0; node.vx = 0; node.vy = 0; node.fixed = false; node.placed = false; }
      const pad = node.type === "machine" ? 12 : 10;
      node.w = 16 + node.label.length * 6.8 + pad;
      node.h = 22;
      next.set(id, node);
    }
    nodes = next;
    edges = wantEdges;

    svg.replaceChildren();
    const edgeLayer  = svgEl("g", {});
    const labelLayer = svgEl("g", {});
    const nodeLayer  = svgEl("g", {});
    svg.append(edgeLayer, labelLayer, nodeLayer);

    for (const e of edges) {
      e.line = svgEl("line", { class: `g-edge ${e.cls}` });
      if (e.cls === "e-runs") {
        e.line.classList.add(...runsHealth(e.units).split(" ").filter(Boolean));
        const t = svgEl("title", {}); t.textContent = unitTitle(e.units); e.line.append(t);
      }
      edgeLayer.append(e.line);
      if (e.label) {
        e.text = svgEl("text", { class: "g-elabel" });
        e.text.textContent = e.label;
        labelLayer.append(e.text);
      }
    }

    for (const node of nodes.values()) {
      const g = svgEl("g", {
        class: `g-node n-${node.type}`, tabindex: "0",
        role: "button", "aria-label": node.label,
      });
      g.append(svgEl("rect", { class: "n-box", x: -node.w / 2, y: -node.h / 2, width: node.w, height: node.h }));
      const ledCls = node.type === "machine" ? `role-${node.role}` : `status-${node.status}`;
      g.append(svgEl("rect", { class: `n-led ${ledCls}`, x: -node.w / 2 + 6, y: -3, width: 6, height: 6 }));
      const label = svgEl("text", { class: "n-label", x: -node.w / 2 + 16, y: 0 });
      label.textContent = node.label;
      g.append(label);
      node.el = g;
      attachDrag(node);
      nodeLayer.append(g);
    }
  }

  function refreshHealth() {
    for (const e of edges) {
      if (e.cls !== "e-runs") continue;
      e.line.setAttribute("class", `g-edge ${e.cls}`);
      e.line.classList.add(...runsHealth(e.units).split(" ").filter(Boolean));
      const t = e.line.querySelector("title");
      if (t) t.textContent = unitTitle(e.units);
    }
  }

  function attachDrag(node) {
    node.el.addEventListener("pointerdown", (ev) => {
      ev.preventDefault();
      drag = { node, moved: false, startX: ev.clientX, startY: ev.clientY };
      node.fixed = true;
      node.el.setPointerCapture(ev.pointerId);
      reheat(0.6);
    });
    node.el.addEventListener("pointermove", (ev) => {
      if (!drag || drag.node !== node) return;
      if (Math.abs(ev.clientX - drag.startX) + Math.abs(ev.clientY - drag.startY) > 4) drag.moved = true;
      const r = svg.getBoundingClientRect();
      node.x = ev.clientX - r.left;
      node.y = ev.clientY - r.top;
      reheat(0.4);
    });
    const end = () => {
      if (!drag || drag.node !== node) return;
      if (!drag.moved) { node.fixed = false; openGraphNode(node); }
      drag = null;
    };
    node.el.addEventListener("pointerup", end);
    node.el.addEventListener("pointercancel", end);
    node.el.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); openGraphNode(node); }
    });
  }

  function openGraphNode(node) { if (node.entry) openNote(node.entry); }
  function reheat(t) { temp = Math.max(temp, t * Math.min(W, H)); start(); }

  function start() {
    if (raf) return;
    const loop = () => {
      step();
      placeFrame();
      if (temp > 0.5 || drag) { raf = requestAnimationFrame(loop); }
      else { raf = null; }
    };
    raf = requestAnimationFrame(loop);
  }

  function stop() { if (raf) { cancelAnimationFrame(raf); raf = null; } }

  function step() {
    const arr = [...nodes.values()];
    const k   = Math.sqrt((W * H) / Math.max(arr.length, 1)) * 0.78;
    const cx  = W / 2, cy = H / 2;
    for (const n of arr) { n.dx = 0; n.dy = 0; }

    for (let i = 0; i < arr.length; i++) {
      for (let j = i + 1; j < arr.length; j++) {
        const a = arr[i], b = arr[j];
        let dx = a.x - b.x, dy = a.y - b.y;
        let d  = Math.hypot(dx, dy) || 0.01;
        const f = (k * k) / d;
        dx /= d; dy /= d;
        a.dx += dx * f; a.dy += dy * f;
        b.dx -= dx * f; b.dy -= dy * f;
      }
    }
    for (const e of edges) {
      const a = nodes.get(e.from), b = nodes.get(e.to);
      if (!a || !b) continue;
      let dx = a.x - b.x, dy = a.y - b.y;
      let d  = Math.hypot(dx, dy) || 0.01;
      const f = (d * d) / k;
      dx /= d; dy /= d;
      a.dx -= dx * f; a.dy -= dy * f;
      b.dx += dx * f; b.dy += dy * f;
    }
    for (const n of arr) {
      n.dx += (cx - n.x) * 0.014;
      n.dy += (cy - n.y) * 0.014;
    }
    const padX = 64, padY = 28;
    for (const n of arr) {
      if (n.fixed) continue;
      const d = Math.hypot(n.dx, n.dy);
      if (d > 0) {
        const m = Math.min(d, temp) / d;
        n.x += n.dx * m;
        n.y += n.dy * m;
      }
      n.x = Math.max(padX, Math.min(W - padX, n.x));
      n.y = Math.max(padY, Math.min(H - padY, n.y));
    }
    temp *= 0.96;
  }

  function placeFrame() {
    for (const n of nodes.values())
      n.el.setAttribute("transform", `translate(${n.x.toFixed(1)} ${n.y.toFixed(1)})`);
    for (const e of edges) {
      const a = nodes.get(e.from), b = nodes.get(e.to);
      if (!a || !b) continue;
      e.line.setAttribute("x1", a.x); e.line.setAttribute("y1", a.y);
      e.line.setAttribute("x2", b.x); e.line.setAttribute("y2", b.y);
      if (e.text) { e.text.setAttribute("x", (a.x + b.x) / 2); e.text.setAttribute("y", (a.y + b.y) / 2 - 3); }
    }
  }

  function ensureLayout() {
    const r = frame.getBoundingClientRect();
    W = r.width || 800; H = r.height || 480;
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    const arr = [...nodes.values()];
    arr.forEach((n, i) => {
      if (n.placed) return;
      const ang = (i / arr.length) * Math.PI * 2;
      const rad = Math.min(W, H) * 0.3;
      n.x = W / 2 + Math.cos(ang) * rad + (Math.random() - 0.5) * 40;
      n.y = H / 2 + Math.sin(ang) * rad + (Math.random() - 0.5) * 40;
      n.placed = true;
    });
    reheat(0.9);
  }

  function update(state) {
    services = state.services || [];
    const m = model(state);
    if (!m) { emptyMsg.classList.remove("hidden"); svg.style.display = "none"; sig = null; stop(); return; }
    emptyMsg.classList.add("hidden"); svg.style.display = "";
    if (m.signature !== sig) {
      rebuild(m.wantNodes, m.wantEdges);
      sig = m.signature;
      if (visible) ensureLayout();
    } else {
      refreshHealth();
    }
  }

  function show() { visible = true; if (nodes.size) ensureLayout(); }
  function hide() { visible = false; stop(); }

  window.addEventListener("resize", () => { if (!visible || !nodes.size) return; ensureLayout(); });

  return { update, show, hide };
})();

// ── note overlay ──────────────────────────────────────────────────────────
const overlay = document.getElementById("overlay");

function section(title) {
  const h = document.createElement("h4");
  h.className = "sheet-section";
  h.textContent = title;
  return h;
}

async function openNote(entry) {
  document.getElementById("sheet-title").textContent = entry.name;
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

  if (entry.recent && entry.recent.length) {
    const ul = document.createElement("ul");
    ul.className = "recent";
    for (const c of entry.recent) {
      const li   = document.createElement("li");
      const when = document.createElement("span");
      when.className   = "rc-when";
      when.textContent = relTime(c.date);
      const msg  = document.createElement("span");
      msg.className   = "rc-msg";
      msg.textContent = c.message;
      li.append(when, msg);
      ul.append(li);
    }
    body.append(section("Recent work"), ul);
  }

  const note = document.createElement("div");
  note.className   = "markdown";
  note.textContent = "loading note…";
  body.append(section("Note"), note);
  try {
    const res = await fetch(`${API}/api/note/${encodeURIComponent(entry.name)}`, { cache: "no-store" });
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

// ── footer status ─────────────────────────────────────────────────────────
function setStatus(text, kind) {
  const el = document.getElementById("status");
  el.textContent = text;
  el.className = kind || "";
}

function showError(message) {
  const ul = document.getElementById("activity-feed");
  ul.removeAttribute("aria-busy");
  ul.replaceChildren();
  const li = document.createElement("li");
  li.className = "act-empty";
  li.textContent = "—";
  ul.append(li);
  setStatus(`can't reach harbor (${message}) — is the server up / are you on the tailnet?`, "err");
}

// ── data ──────────────────────────────────────────────────────────────────
let lastState = null;

async function load() {
  if (!API) { showError("no API configured"); return; }
  try {
    const res = await fetch(`${API}/api/state`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const state = await res.json();
    lastState = state;
    buildFocus(state);
    buildActivity(state);
    graph.update(state);
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
  const all  = lastState ? [...(lastState.projects || []), ...(lastState.areas || [])] : [];
  openNote(all.find((e) => e.name === name) || { name });
}
window.addEventListener("hashchange", openFromHash);

// ── view toggle (Main / Graph) ────────────────────────────────────────────
const mainView  = document.getElementById("main-view");
const graphView = document.getElementById("graph-view");
const tabMain   = document.getElementById("tab-main");
const tabGraph  = document.getElementById("tab-graph");

function selectView(which) {
  const isGraph = which === "graph";
  graphView.classList.toggle("hidden", !isGraph);
  mainView.classList.toggle("hidden",  isGraph);
  tabGraph.classList.toggle("active",  isGraph);
  tabMain.classList.toggle("active",   !isGraph);
  tabGraph.setAttribute("aria-selected", String(isGraph));
  tabMain.setAttribute("aria-selected",  String(!isGraph));
  if (isGraph) graph.show(); else graph.hide();
}
tabMain.addEventListener("click",  () => selectView("main"));
tabGraph.addEventListener("click", () => selectView("graph"));

// ── boot ──────────────────────────────────────────────────────────────────
tick();
setInterval(tick, 1000 * 15);
initCapture();
load().then(openFromHash);
setInterval(load, 1000 * 60);
