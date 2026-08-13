// bridge/lib/session-store.js
// Zero-dependency reader/mapper for pi session files (v3 JSONL tree format).
//
// A session file is an append-only tree of entries linked by id/parentId.
// The current branch is the chain from the newest entry (the leaf) back to
// the root; entries on abandoned branches exist in the file but are not part
// of the conversation and must never surface.
//
// Files are written by a live pi process, so the last line can be a half-
// written JSON object. Every line is parsed independently and unparseable
// lines are skipped; the "leaf" is the last line that parsed.
//
// This module is deliberately pure — no process state, no caching — so the
// HTTP layer owns the composition of file content with the live-streaming
// overlay (see lib/pi-rpc.js).

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";

// Tool output is joined into one part field; a hard cap keeps a single 1MB
// bash dump from ballooning every transcript response.
const TOOL_OUTPUT_TRUNCATION = 2000;

// The scan dir follows pi's own resolution so the default deployment scans
// exactly what pi writes: PI_SESSION_DIR (bridge override) wins, then pi's
// own override, then the platform default. See lib/pi-rpc.js for why the
// override also switches the spawned processes onto --session-dir.
export function defaultSessionDir() {
  return (
    process.env.PI_SESSION_DIR ||
    process.env.PI_CODING_AGENT_SESSION_DIR ||
    path.join(os.homedir(), ".pi", "agent", "sessions")
  );
}

// JSONL discipline: each record is one line, LF-delimited, with an optional
// trailing CR tolerated (pi writes LF; the RPC framing spec allows CRLF).
// Exported for the stream registry, which parses tailed lines with the same
// discipline as the file reader.
export function parseLine(line) {
  try {
    return JSON.parse(line.endsWith("\r") ? line.slice(0, -1) : line);
  } catch {
    return null; // a partial line while pi is mid-write, or a corrupt file
  }
}

// The entry timestamp is an ISO string in the file. The message payload also
// carries a Unix-ms timestamp, but the entry one is what the transcript is
// ordered by, so that is what surfaces.
function timestampMs(entry) {
  if (entry?.timestamp == null) return null;
  if (typeof entry.timestamp === "number") return entry.timestamp;
  const ms = Date.parse(entry.timestamp);
  return Number.isNaN(ms) ? null : ms;
}

// Recursive walk of the session dir. The dir layout is pi's per-cwd naming
// (`--<cwd with / replaced by ->--`) but with --session-dir pi writes flat, so
// the walk must be recursive regardless; both shapes must work.
async function walkJsonlFiles(dir) {
  const out = [];
  async function walk(current) {
    let entries;
    try {
      entries = await fs.readdir(current, { withFileTypes: true });
    } catch {
      return; // a missing or unreadable session dir simply has no sessions
    }
    for (const entry of entries) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) await walk(full);
      else if (entry.isFile() && entry.name.endsWith(".jsonl")) out.push(full);
    }
  }
  await walk(dir);
  return out;
}

// id -> absolute file path, without reading any content (a session id is just
// the file basename, so resolution is a cheap name walk).
export async function resolveSessionFile(sessionDir, id) {
  const target = `${id}.jsonl`;
  for (const file of await walkJsonlFiles(sessionDir)) {
    if (path.basename(file) === target) return file;
  }
  return null;
}

// Parse one session file into { header, entries }. Returns null when the file
// is unreadable or its first line is not a session header — such a file is
// not a session, and callers treat it as an unknown id.
export async function readSessionFile(filePath) {
  let text;
  try {
    text = await fs.readFile(filePath, "utf8");
  } catch {
    return null;
  }
  let header = null;
  const entries = [];
  for (const line of text.split("\n")) {
    const entry = parseLine(line);
    if (!entry) continue;
    if (entry.type === "session") {
      header ??= entry; // the header is line 1; keep the first one seen
      continue;
    }
    if (typeof entry.id === "string") entries.push(entry);
  }
  if (!header) return null;
  return { header, entries };
}

// The leaf branch: the chain from the newest entry (the leaf) up to the root,
// reversed into chronological order. The cycle guard exists because a corrupt
// parentId chain must not hang the reader forever.
function leafPathEntries(entries) {
  if (entries.length === 0) return [];
  const byId = new Map(entries.map((entry) => [entry.id, entry]));
  const chain = [];
  const seen = new Set();
  let current = entries[entries.length - 1];
  while (current && !seen.has(current.id)) {
    seen.add(current.id);
    chain.push(current);
    current = byId.get(current.parentId) ?? null;
  }
  chain.reverse();
  return chain;
}

function latestOnPath(leafPath, predicate) {
  // Walk leaf -> root; the first match is the most recent occurrence.
  for (let i = leafPath.length - 1; i >= 0; i--) {
    if (predicate(leafPath[i])) return leafPath[i];
  }
  return null;
}

export async function readSessionFromFile(filePath) {
  const parsed = await readSessionFile(filePath);
  if (!parsed) return null;
  return buildSessionObject(filePath, parsed);
}

// The orchestrator mode as last recorded in the session: the orchestrator
// extension appends an "orchestrator-mode" custom entry on every runtime
// toggle (pi.appendEntry) and restores from it on session start, so the last
// such entry on the leaf branch is the mode's shared record — visible to any
// reader of the file, live process or not. Exported for the stream registry,
// which rebuilds its transcript state from a tailed entry list.
export function orchestratorActiveFromEntries(entries) {
  const entry = latestOnPath(
    leafPathEntries(entries),
    (e) => e.type === "custom" && e.customType === "orchestrator-mode"
  );
  return entry?.data?.active === true;
}

export function buildSessionObject(filePath, { header, entries }) {
  const leafPath = leafPathEntries(entries);
  const lastEntry = entries.length > 0 ? entries[entries.length - 1] : null;
  const nameEntry = latestOnPath(leafPath, (e) => e.type === "session_info" && typeof e.name === "string" && e.name !== "");
  const modelEntry = latestOnPath(leafPath, (e) => e.type === "model_change" && typeof e.modelId === "string");
  const created = timestampMs(header);
  return {
    id: path.basename(filePath, ".jsonl"),
    title: nameEntry?.name ?? null,
    directory: typeof header.cwd === "string" ? header.cwd : null,
    time: {
      created,
      updated: lastEntry ? (timestampMs(lastEntry) ?? created) : created,
    },
    model: modelEntry
      ? { id: modelEntry.modelId }
      : modelFromLastAssistant(leafPath),
    orchestrator: { active: orchestratorActiveFromEntries(entries) },
  };
}

// Fallback when the session has no model_change entry: the model of the last
// assistant message on the active branch.
function modelFromLastAssistant(leafPath) {
  const assistant = latestOnPath(
    leafPath,
    (e) => e.type === "message" && e.message?.role === "assistant" && typeof e.message.model === "string"
  );
  return assistant ? { id: assistant.message.model } : null;
}

// GET /session/{id}/message: the leaf branch mapped to skiff's { info, parts }
// transcript shape.
export async function readMessagesFromFile(filePath) {
  const parsed = await readSessionFile(filePath);
  if (!parsed) return null;
  return mapMessages(parsed.entries);
}

export async function listSessions(sessionDir) {
  const sessions = [];
  for (const file of await walkJsonlFiles(sessionDir)) {
    const session = await readSessionFromFile(file);
    if (session) sessions.push(session);
  }
  // Most-recently-active first; skiff re-sorts client-side anyway.
  sessions.sort((a, b) => (b.time?.updated ?? 0) - (a.time?.updated ?? 0));
  return sessions;
}

// Map a session's entries to the transcript shape. Exported so the HTTP layer
// can reuse an already-parsed file (the server resolves a session target once
// per request and must not read the same file twice while pi appends to it).
export function mapMessages(entries) {
  const messages = [];
  // toolCall parts are emitted by assistant messages; toolResult entries then
  // fold into them by id, so a tool's status/output lives on one part.
  const toolPartsByCallId = new Map();
  for (const entry of leafPathEntries(entries)) {
    if (entry.type === "message" && entry.message?.role === "toolResult") {
      const standalone = foldToolResult(entry, toolPartsByCallId);
      if (standalone) messages.push(standalone);
      continue;
    }
    const mapped = mapEntry(entry);
    if (!mapped) continue;
    for (const part of mapped.parts) {
      if (part.type === "tool" && part.id) toolPartsByCallId.set(part.id, part);
    }
    messages.push(mapped);
  }
  return messages;
}

// Map one tree entry to { info, parts }, or null when the entry type has no
// transcript rendering.
//
// Rendering policy for non-message entries:
//   compaction / branch_summary -> one text part with the summary
//   custom_message              -> one text part with the content
//   bashExecution               -> skipped (a bare shell command's echo adds
//                                  noise skiff's transcript has no slot for)
//   custom, label, thinking_level_change, model_change, session_info -> skipped
//     (custom/label/thinking_level_change carry no user-facing prose;
//     model_change/session_info are metadata surfaced in the session object)
// A bashExecution's output only becomes visible through the assistant text
// that references it, which is how the TUI reads it too.
function mapEntry(entry) {
  if (entry.type === "message") {
    const role = entry.message?.role;
    if (role === "user" || role === "assistant") return mapMessageEntry(entry);
    if (role === "branchSummary") return syntheticTextEntry(entry, entry.message.summary);
    if (role === "compactionSummary") return syntheticTextEntry(entry, entry.message.summary);
    if (role === "custom") return syntheticTextEntry(entry, contentToText(entry.message.content));
    return null; // toolResult (folded upstream), bashExecution, unknown roles
  }
  if (entry.type === "compaction" || entry.type === "branch_summary") {
    return syntheticTextEntry(entry, entry.summary);
  }
  if (entry.type === "custom_message") return syntheticTextEntry(entry, contentToText(entry.content));
  return null;
}

// Map a user/assistant message entry. `streaming` marks the live overlay
// entry (see PiProcess.getPendingMessage): its info carries no time.completed
// so skiff's working-indicator fallback keeps firing while it streams.
export function mapMessageEntry(entry, { streaming = false } = {}) {
  const msg = entry.message ?? {};
  const role = msg.role;
  const created = timestampMs(entry);
  const info = { id: entry.id, role, time: { created } };
  if (role === "assistant") {
    info.agent = typeof msg.model === "string" ? msg.model : null;
    if (!streaming) info.time.completed = created;
  }
  const blocks = normalizeContent(msg.content);
  const parts = [];
  for (const block of blocks) {
    switch (block.type) {
      case "text":
        parts.push({ type: "text", text: block.text ?? "", id: partId(entry, parts) });
        break;
      case "thinking":
        parts.push({ type: "reasoning", text: block.thinking ?? "", id: partId(entry, parts) });
        break;
      case "image":
        // skiff renders image blocks as inert file lines; there is no image
        // transport in the transcript, only the filename (display-only).
        parts.push({ type: "file", filename: filenameForImage(block), id: partId(entry, parts) });
        break;
      case "toolCall":
        // A tool part starts "running"; the matching toolResult folds into it
        // (see foldToolResult) flipping status and attaching output.
        parts.push({ type: "tool", tool: block.name ?? "", id: block.id, state: { status: "running" } });
        break;
      default:
        break; // unknown content blocks render nothing
    }
  }
  return { info, parts };
}

// Part ids must be unique within a message: skiff's poller fingerprints the
// transcript and sorts parts by id, and the pending overlay reuses this
// function with entry id "<pending>".
function partId(entry, parts) {
  return `${entry.id}-p${parts.length}`;
}

function normalizeContent(content) {
  if (typeof content === "string") return [{ type: "text", text: content }];
  if (Array.isArray(content)) return content.filter((block) => block && typeof block.type === "string");
  return [];
}

function filenameForImage(block) {
  // mimeType "image/png" -> image.png; "image/svg+xml" -> image.svg. The
  // filename is display-only; pi stores images as base64 data without names.
  const subtype = String(block.mimeType ?? "").split("/")[1] ?? "";
  const ext = subtype.split("+")[0] || "img";
  return `image.${ext}`;
}

// A toolResult folds into the tool part its assistant message emitted. The
// standalone path (no matching toolCall on the branch — the call lives on an
// abandoned branch, or the file is mid-write) still surfaces the outcome as
// its own message rather than silently dropping it.
function foldToolResult(entry, toolPartsByCallId) {
  const msg = entry.message ?? {};
  const existing = toolPartsByCallId.get(msg.toolCallId);
  const status = msg.isError ? "error" : "completed";
  const output = joinToolOutput(msg.content);
  if (existing) {
    existing.state.status = status;
    existing.state.output = output;
    return null;
  }
  return {
    info: { id: entry.id, role: "toolResult", time: { created: timestampMs(entry) } },
    parts: [{ type: "tool", tool: msg.toolName ?? "", id: msg.toolCallId, state: { status, output } }],
  };
}

function joinToolOutput(content) {
  const text = normalizeContent(content)
    .filter((block) => block.type === "text")
    .map((block) => block.text ?? "")
    .join("\n");
  if (text.length <= TOOL_OUTPUT_TRUNCATION) return text;
  return text.slice(0, TOOL_OUTPUT_TRUNCATION) + "…";
}

// Compaction/branch-summary/custom-message entries render as agent-side prose.
// They get role "assistant" (skiff labels anything non-user "opencode") with
// time.completed set, so skiff's working fallback — "newest assistant message
// without time.completed" — cannot mistake a finished summary for a live
// stream.
function syntheticTextEntry(entry, text) {
  const created = timestampMs(entry);
  return {
    info: { id: entry.id, role: "assistant", agent: null, time: { created, completed: created } },
    parts: text
      ? [{ type: "text", text, id: `${entry.id}-p0` }]
      : [],
  };
}

function contentToText(content) {
  return normalizeContent(content)
    .filter((block) => block.type === "text")
    .map((block) => block.text ?? "")
    .join("\n");
}
