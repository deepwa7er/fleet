// driftword UI — build a query from the controls, fetch /api/generate, render a
// dense results table. No framework, no build step (matches the fleet ethos).

const $ = (id) => document.getElementById(id);

let currentWords = [];

function setStatus(text, kind) {
  const el = $("status");
  el.textContent = text;
  el.className = kind || "";
}

function showError(message) {
  const el = $("error");
  el.textContent = `ERROR · ${message}`;
  el.classList.remove("hidden");
}
function clearError() {
  $("error").classList.add("hidden");
}

function buildQuery() {
  const mode = document.querySelector('input[name="mode"]:checked').value;
  const p = new URLSearchParams();
  p.set("mode", mode);
  p.set("count", $("count").value || "20");
  p.set("min", $("min").value || "1");
  p.set("max", $("max").value || "10");
  p.set("order", $("order").value || "3");
  p.set("syllables", $("syllables").value || "2");
  const prefer = $("prefer").value.trim();
  if (prefer) p.set("prefer", prefer);
  p.set("strength", $("strength").value || "4");
  if ($("only").checked) p.set("only", "true");
  const seed = $("seed").value.trim();
  if (seed !== "") p.set("seed", seed);
  return p;
}

function pad2(n) {
  return String(n).padStart(2, "0");
}

function nowStamp() {
  const d = new Date();
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
}

function renderMeta(params) {
  const bits = [
    `GENERATED ${nowStamp()}`,
    params.mode,
    params.mode === "markov" ? `order ${params.order}` : `${params.syllables} syll`,
    `len ${params.min}–${params.max}`,
    `n=${params.count}`,
  ];
  if (params.prefer) bits.push(`prefer ${params.prefer}${params.only ? " (only)" : ""}×${params.strength}`);
  bits.push(params.seed === null || params.seed === undefined ? "seed random" : `seed ${params.seed}`);
  $("meta").textContent = bits.join(" · ");
}

function renderRows(words) {
  const tbody = $("rows");
  tbody.replaceChildren();
  if (!words.length) {
    const tr = document.createElement("tr");
    tr.className = "placeholder";
    const td = document.createElement("td");
    td.colSpan = 3;
    td.textContent = "No words matched the constraints.";
    tr.append(td);
    tbody.append(tr);
    return;
  }
  for (const row of words) {
    const tr = document.createElement("tr");
    tr.className = "word-row";

    const idx = document.createElement("td");
    idx.className = "num";
    idx.textContent = row.index;

    const w = document.createElement("td");
    w.className = "w";
    w.textContent = row.word;

    const len = document.createElement("td");
    len.className = "num";
    len.textContent = row.length;

    tr.append(idx, w, len);
    tr.addEventListener("click", () => copyWord(tr, row.word));
    tbody.append(tr);
  }
}

async function copyWord(tr, word) {
  try {
    await navigator.clipboard.writeText(word);
    document.querySelectorAll("tr.copied").forEach((el) => el.classList.remove("copied"));
    tr.classList.add("copied");
  } catch {
    /* clipboard blocked; ignore — the word is visible regardless */
  }
}

async function copyAll() {
  if (!currentWords.length) return;
  try {
    await navigator.clipboard.writeText(currentWords.map((w) => w.word).join("\n"));
    setStatus(`copied ${currentWords.length} words`, "ok");
  } catch {
    setStatus("clipboard unavailable", "err");
  }
}

async function generate(ev) {
  if (ev) ev.preventDefault();
  clearError();
  setStatus("generating…", "");
  const query = buildQuery();
  try {
    const res = await fetch(`api/generate?${query.toString()}`, { cache: "no-store" });
    const data = await res.json();
    if (!res.ok) {
      showError(data.error || `HTTP ${res.status}`);
      setStatus("rejected", "err");
      return;
    }
    currentWords = data.words;
    if (typeof data.corpus === "number") {
      $("corpus").textContent = `CORPUS · ${data.corpus.toLocaleString()}`;
    }
    renderMeta(data.params);
    renderRows(data.words);
    setStatus(`${data.words.length} words`, "ok");
  } catch (e) {
    showError(e.message || String(e));
    setStatus("request failed", "err");
  }
}

$("controls").addEventListener("submit", generate);
$("copy").addEventListener("click", copyAll);

// Generate once on load so the page is never empty.
generate();
