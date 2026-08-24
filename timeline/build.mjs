#!/usr/bin/env node
// timeline/build.mjs — the rendered timeline (DW-003 §4).
//
// A zero-dependency static generator over the record repository: read every
// entry (<record>/<repo>/<card>.json), write a static site — index.html (the
// timeline, newest first) plus one page per entry (the annotated change:
// code alongside the reasoning for it). A rebuild is idempotent from the
// record alone; there is no server and no state.
//
// Everything rendered here is already public by construction — the privacy
// boundary is build_public_change in crates/change/src/record.rs, enforced at
// export.
// This file still escapes every string it interpolates, because "already
// filtered" is a provenance claim, not an HTML-safety property.
//
// Usage: node build.mjs [--record <dir>] [--out <dir>]
//   record dir default: ~/code/record  (RECORD_DIR overrides)
//   out dir default:    ./dist         (TIMELINE_OUT overrides)

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

function argValue(flag) {
  const at = process.argv.indexOf(flag);
  return at !== -1 ? process.argv[at + 1] : undefined;
}

const RECORD_DIR = argValue("--record") ?? process.env.RECORD_DIR ?? path.join(os.homedir(), "code", "record");
const OUT_DIR = argValue("--out") ?? process.env.TIMELINE_OUT ?? path.join(HERE, "dist");

// --- Reading the record ------------------------------------------------------

export async function readEntries(recordDir) {
  const entries = [];
  let repos;
  try {
    repos = await fs.readdir(recordDir, { withFileTypes: true });
  } catch (err) {
    throw new Error(`record directory unreadable: ${recordDir}: ${err.message}`);
  }
  for (const repo of repos) {
    if (!repo.isDirectory() || repo.name.startsWith(".")) continue;
    for (const file of await fs.readdir(path.join(recordDir, repo.name))) {
      if (!file.endsWith(".json")) continue;
      const raw = await fs.readFile(path.join(recordDir, repo.name, file), "utf8");
      let entry;
      try {
        entry = JSON.parse(raw);
      } catch {
        console.error(`skipping unparseable entry: ${repo.name}/${file}`);
        continue; // one corrupt entry must not take the timeline down
      }
      entries.push(entry);
    }
  }
  entries.sort((a, b) => (a.landedAt < b.landedAt ? 1 : -1));
  return entries;
}

// --- Git-diff parsing (the annotation coordinate system) ---------------------

export function parseDiff(text) {
  const files = [];
  let file = null;
  let hunk = null;
  let oldNumber = 0;
  let newNumber = 0;
  // split("\n") on text ending with a newline yields one trailing empty
  // string — an artifact, not an empty context line; drop it before parsing.
  const rawLines = String(text ?? "").split("\n");
  if (rawLines.at(-1) === "") rawLines.pop();
  for (const line of rawLines) {
    const fileMatch = /^diff --git a\/(.*) b\/(.*)$/.exec(line);
    if (fileMatch) {
      file = { oldPath: fileMatch[1], newPath: fileMatch[2], binary: false, hunks: [] };
      files.push(file);
      hunk = null;
      continue;
    }
    if (!file) continue;
    const hunkMatch = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (hunkMatch) {
      hunk = { header: line, lines: [] };
      file.hunks.push(hunk);
      oldNumber = Number(hunkMatch[1]);
      newNumber = Number(hunkMatch[2]);
      continue;
    }
    if (line.startsWith("Binary files ")) {
      file.binary = true;
      continue;
    }
    if (!hunk) continue;
    const kind = line[0];
    if (kind === "+") {
      hunk.lines.push({ kind: "add", oldNumber: null, newNumber, text: line.slice(1) });
      newNumber += 1;
    } else if (kind === "-") {
      hunk.lines.push({ kind: "del", oldNumber, newNumber: null, text: line.slice(1) });
      oldNumber += 1;
    } else if (kind === " " || kind === undefined) {
      hunk.lines.push({ kind: "context", oldNumber, newNumber, text: line.slice(1) });
      oldNumber += 1;
      newNumber += 1;
    } else if (kind === "\\") {
      // "\ No newline at end of file" — a marker, not a line of either side.
    } else {
      hunk = null; // trailing non-diff content ends the hunk, never crashes it
    }
  }
  return files;
}

// --- HTML --------------------------------------------------------------------

export function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

const e = escapeHtml;

function landedDate(entry) {
  return String(entry.landedAt ?? "").slice(0, 10);
}

function entryHref(entry) {
  return `${encodeURIComponent(entry.repo)}/${entry.card}.html`;
}

function instrumentation(entry) {
  const rounds = (entry.rounds ?? []).length;
  return `${e(entry.repo)} #${e(entry.card)} · ${rounds} round${rounds === 1 ? "" : "s"} · ${e(landedDate(entry))} · ${e(String(entry.tip ?? "").slice(0, 12))}`;
}

// Annotations for one rendered line: an added line anchors the new side, a
// deleted line the old side, a context line both. Same coordinate rules as
// skiff's review (ChangeHelper#anchors_for).
function annotationsAt(annotations, file, line) {
  return (annotations ?? []).filter((annotation) => {
    if (annotation.side === "new") return annotation.path === file.newPath && annotation.line === line.newNumber;
    return annotation.path === file.oldPath && annotation.line === line.oldNumber;
  });
}

function renderRound(round) {
  const files = parseDiff(round.diff);
  const placed = new Set();
  const parts = [];
  const claims = (round.gatesRan ?? []).length
    ? `<p class="instrumentation">agent ran ${e((round.gatesRan ?? []).join(" · "))}</p>`
    : "";
  const worth = (round.worthKnowing ?? []).length
    ? `<div class="worth-knowing"><p class="instrumentation">worth knowing</p><ul>${(round.worthKnowing ?? [])
        .map((item) => `<li>${e(item)}</li>`)
        .join("")}</ul></div>`
    : "";
  parts.push(`<section class="round">`);
  parts.push(`<p class="instrumentation">round ${e(round.n)} · ${e(round.author)} · ${e(String(round.commit ?? "").slice(0, 12))}</p>`);
  parts.push(claims, worth);
  for (const file of files) {
    parts.push(`<section class="diff-file"><p class="instrumentation diff-file-header">${e(
      file.oldPath === file.newPath ? file.newPath : `${file.oldPath} → ${file.newPath}`
    )}</p>`);
    if (file.binary) parts.push(`<p class="instrumentation">binary file</p>`);
    parts.push(`<div class="diff-scroll">`);
    for (const hunk of file.hunks) {
      parts.push(`<p class="diff-hunk-header">${e(hunk.header)}</p>`);
      for (const line of hunk.lines) {
        parts.push(
          `<div class="diff-line diff-line--${line.kind}"><span class="diff-gutter">${line.oldNumber ?? ""}</span><span class="diff-gutter">${line.newNumber ?? ""}</span><span class="diff-text">${e(line.text)}</span></div>`
        );
        for (const annotation of annotationsAt(round.annotations, file, line)) {
          placed.add(annotation);
          parts.push(`<aside class="annotation">${e(annotation.text)}</aside>`);
        }
      }
    }
    parts.push(`</div></section>`);
  }
  const stranded = (round.annotations ?? []).filter((annotation) => !placed.has(annotation));
  if (stranded.length) {
    parts.push(`<section class="diff-file"><p class="instrumentation">annotations whose lines this diff no longer shows</p>`);
    for (const annotation of stranded) {
      parts.push(
        `<aside class="annotation"><span class="instrumentation">${e(annotation.path)}:${e(annotation.line)}</span> ${e(annotation.text)}</aside>`
      );
    }
    parts.push(`</section>`);
  }
  parts.push(`</section>`);
  return parts.join("\n");
}

function page(title, body, { depth = 0 } = {}) {
  const home = depth === 0 ? "index.html" : "../index.html";
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${e(title)}</title>
<style>${CSS}</style>
</head>
<body>
<div class="container">
<header class="masthead"><a href="${home}">the record</a></header>
${body}
</div>
</body>
</html>
`;
}

export function renderEntryPage(entry) {
  const title = entry.title ?? `change #${entry.card}`;
  const rounds = (entry.rounds ?? []).map(renderRound).join("\n");
  const body = `<main>
<h1>${e(title)}</h1>
<p class="instrumentation">${instrumentation(entry)}</p>
${rounds}
</main>`;
  return page(title, body, { depth: 1 });
}

export function renderIndex(entries) {
  const items = entries
    .map(
      (entry) =>
        `<li class="item"><a class="item-title" href="${entryHref(entry)}">${e(entry.title ?? `change #${entry.card}`)}</a><p class="instrumentation">${instrumentation(entry)}</p></li>`
    )
    .join("\n");
  const body = `<main>
<p class="instrumentation">${entries.length} shipped change${entries.length === 1 ? "" : "s"}</p>
${entries.length ? `<ul class="list">${items}</ul>` : `<p class="prose">Nothing shipped yet — the record starts at the workflow cutover.</p>`}
</main>`;
  return page("the record", body);
}

// DW-001 in miniature: the six rules for a read-only document. Whitespace
// separates (no borders); nothing here is pressable, so nothing has depth;
// warm paper / cool charcoal; the one blue is links only; metadata is
// instrumentation; no motion at all. Diff washes reuse --good/--danger —
// the diff precedent, never a second color set.
const CSS = `
:root {
  color-scheme: light dark;
  --bg: #f7f2e9; --fill: #fffdf8; --text: #1f1a12; --text-muted: #7a7264;
  --accent: #0066b1; --danger: #dc2626; --good: #2f6f3e;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #121316; --fill: #1c1e22; --text: #eceef1; --text-muted: #8b9199;
    --accent: #4d9de0; --danger: #f87171; --good: #5cb974;
  }
}
* { margin: 0; padding: 0; box-sizing: border-box; }
body { background: var(--bg); color: var(--text); font-family: Georgia, serif; line-height: 1.6; }
.container { max-width: 44rem; margin: 0 auto; padding: 1.5rem 1.25rem 4rem; }
.masthead { margin-bottom: 2.5rem; }
.masthead a { color: var(--text); text-decoration: none; font-size: 0.85rem; letter-spacing: 0.14em; text-transform: uppercase; }
h1 { font-size: 1.6rem; font-weight: 600; letter-spacing: -0.01em; line-height: 1.4; margin-bottom: 0.35rem; }
.instrumentation { font-family: ui-monospace, monospace; font-size: 0.7rem; font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase; font-variant-numeric: tabular-nums; color: var(--text-muted); }
.prose { margin-top: 1.5rem; }
.list { list-style: none; margin-top: 2rem; }
.item { margin-bottom: 1.75rem; }
.item-title { color: var(--accent); text-decoration: none; font-size: 1.1rem; }
.item-title:hover { text-decoration: underline; }
.round { margin-top: 2.5rem; }
.worth-knowing ul { margin: 0.25rem 0 0 1.1rem; font-size: 0.85rem; color: var(--text-muted); }
.diff-file { background: var(--fill); border-radius: 6px; margin: 0.75rem 0; padding: 0.5rem 0; }
.diff-file-header { padding: 0.25rem 0.75rem; }
.diff-scroll { overflow-x: auto; }
.diff-hunk-header { font-family: ui-monospace, monospace; font-size: 0.7rem; color: var(--text-muted); margin: 0.5rem 0 0.25rem; padding: 0 0.75rem; }
.diff-line { display: flex; font-family: ui-monospace, monospace; font-size: 0.8rem; line-height: 1.5; white-space: pre; }
.diff-gutter { flex: none; width: 2.75rem; padding-right: 0.5rem; text-align: right; color: var(--text-muted); font-variant-numeric: tabular-nums; user-select: none; }
.diff-text { padding-left: 0.5rem; }
.diff-line--add { background: color-mix(in srgb, var(--good) 14%, transparent); }
.diff-line--del { background: color-mix(in srgb, var(--danger) 12%, transparent); }
.annotation { margin: 0.25rem 0.75rem 0.5rem 3.25rem; padding: 0.35rem 0 0.35rem 0.75rem; border-left: 2px solid var(--accent); font-size: 0.85rem; color: var(--text-muted); white-space: normal; overflow-wrap: anywhere; }
`;

// --- Build -------------------------------------------------------------------

export async function build(recordDir, outDir) {
  const entries = await readEntries(recordDir);
  await fs.rm(outDir, { recursive: true, force: true });
  await fs.mkdir(outDir, { recursive: true });
  await fs.writeFile(path.join(outDir, "index.html"), renderIndex(entries));
  for (const entry of entries) {
    const dir = path.join(outDir, entry.repo);
    await fs.mkdir(dir, { recursive: true });
    await fs.writeFile(path.join(dir, `${entry.card}.html`), renderEntryPage(entry));
  }
  return entries.length;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  build(RECORD_DIR, OUT_DIR).then(
    (count) => console.error(`timeline: ${count} entries → ${OUT_DIR}`),
    (err) => {
      console.error(err.message);
      process.exit(1);
    }
  );
}
