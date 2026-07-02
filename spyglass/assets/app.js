"use strict";

// --- Fleet theme sync ------------------------------------------------------
// Honor tide (the fleet-wide dark/light setting) like the React UIs' theme.ts:
// the cookie gave the first paint; here we reconcile with tide and poll for
// changes. Derived from the hostname (tide.<base-domain>), so it no-ops on
// localhost where there's no tide.
(function themeSync() {
  const host = location.hostname;
  const dot = host.indexOf(".");
  if (dot < 0) return; // localhost / bare host — keep the cookie/default
  const tideUrl = "https://tide." + host.slice(dot + 1) + "/theme";

  async function poll() {
    try {
      const r = await fetch(tideUrl, { cache: "no-store" });
      if (r.ok) {
        const { theme } = await r.json();
        if (theme === "dark" || theme === "light") {
          document.documentElement.dataset.theme = theme;
        }
      }
    } catch {
      /* tide unreachable — keep the current theme */
    }
  }
  poll();
  setInterval(poll, 5000);
})();

// --- Search ----------------------------------------------------------------
const form = document.getElementById("search");
const input = document.getElementById("q");
const results = document.getElementById("results");
const statusEl = document.getElementById("status");
const footStatus = document.getElementById("footstatus");
const sourcesTag = document.getElementById("sources");

const enc = new TextEncoder();
const dec = new TextDecoder();

let inflight = 0; // generation counter so a slow response can't clobber a newer one

form.addEventListener("submit", (e) => {
  e.preventDefault();
  runSearch(input.value);
});

async function runSearch(query) {
  query = query.trim();
  results.replaceChildren();
  if (!query) {
    statusEl.textContent = "Enter a query to search across the fleet.";
    return;
  }
  const gen = ++inflight;
  statusEl.textContent = `Searching for “${query}”…`;
  setFoot("searching", false);
  try {
    const r = await fetch(`/api/search?q=${encodeURIComponent(query)}`);
    if (!r.ok) throw new Error(`search failed: ${r.status}`);
    const data = await r.json();
    if (gen !== inflight) return; // a newer search superseded this one
    render(data);
  } catch (err) {
    if (gen !== inflight) return;
    statusEl.textContent = "";
    setFoot(String(err), true);
  }
}

function render(data) {
  results.replaceChildren();
  const total = data.sources.reduce((n, s) => n + s.hits.length, 0);
  const live = data.sources.filter((s) => !s.error).length;
  sourcesTag.textContent = `SOURCES · ${live}/${data.sources.length}`;
  statusEl.textContent = `${total} result${total === 1 ? "" : "s"} for “${data.query}”`;
  setFoot("ready", false);

  for (const source of data.sources) {
    results.append(renderGroup(source));
  }
}

function renderGroup(source) {
  const group = el("div", "group");
  const head = el("div", "group-head");
  head.append(el("span", "group-label", source.label));
  if (!source.error) {
    head.append(el("span", "count", `${source.hits.length}${source.truncated ? "+" : ""}`));
  }
  if (source.error) {
    head.append(el("span", "group-err", "unavailable"));
  } else if (source.truncated) {
    head.append(el("span", "group-trunc", "more matches — refine the query"));
  }
  group.append(head);

  if (source.error) {
    group.append(el("div", "group-empty", `${source.label} couldn't be reached.`));
  } else if (source.hits.length === 0) {
    group.append(el("div", "group-empty", "No matches."));
  } else {
    for (const hit of source.hits) group.append(renderHit(hit));
  }
  return group;
}

function renderHit(hit) {
  // A hit is a link when it has a URL (code → GitHub, tickets/docs → their site),
  // else a plain block (notes have no per-item URL).
  const node = hit.url ? el("a", "hit") : el("div", "hit");
  if (hit.url) {
    node.href = hit.url;
    node.target = "_blank";
    node.rel = "noopener noreferrer";
  }

  // A heading: the location/title (code "repo/path:line", a ticket title, a doc
  // name), or — when there's none (notes) — the item's date.
  if (hit.title) {
    const loc = el("div", "hit-loc");
    loc.textContent = hit.title;
    if (hit.line != null) {
      const ln = el("span", "ln");
      ln.textContent = `:${hit.line}`;
      loc.append(ln);
    }
    node.append(loc);
  } else if (hit.at != null) {
    node.append(el("div", "hit-date", formatDate(hit.at)));
  }

  const snippet = el("div", "hit-snippet");
  snippet.append(highlight(hit.snippet, hit.ranges));
  node.append(snippet);
  return node;
}

// Build a text fragment with the given byte-offset ranges wrapped in <mark>.
// Offsets are UTF-8 byte offsets (from ripgrep / FTS), so we slice on encoded
// bytes and decode each piece — keeping highlights aligned even with non-ASCII.
// textContent does the HTML escaping, so this is XSS-safe by construction.
function highlight(text, ranges) {
  const frag = document.createDocumentFragment();
  const bytes = enc.encode(text);
  const spans = (ranges || [])
    .filter((r) => r.len > 0)
    .sort((a, b) => a.start - b.start);
  let pos = 0;
  for (const r of spans) {
    const start = Math.max(pos, r.start);
    const end = Math.min(bytes.length, r.start + r.len);
    if (start >= end) continue;
    if (start > pos) frag.append(dec.decode(bytes.slice(pos, start)));
    const mark = document.createElement("mark");
    mark.textContent = dec.decode(bytes.slice(start, end));
    frag.append(mark);
    pos = end;
  }
  if (pos < bytes.length) frag.append(dec.decode(bytes.slice(pos)));
  return frag;
}

function formatDate(ms) {
  return new Date(ms).toLocaleString([], {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function setFoot(text, isErr) {
  footStatus.textContent = text;
  footStatus.classList.toggle("err", !!isErr);
}
