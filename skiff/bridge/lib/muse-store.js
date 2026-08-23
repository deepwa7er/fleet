// bridge/lib/muse-store.js
// Zero-dependency reader/mapper for Muse Code session logs.
//
// A muse session is a directory — sessions/YYYY/MM/DD/<uuid>/ under muse's
// data dir — holding an append-only, event-sourced session.jsonl. Every line
// is an envelope { id, stream, sequence, recorded_at (µs), record_type,
// payload_type, payload }; the transcript is carried by a small subset of
// payload types (verified against Muse Code 0.2.1, both TUI and `muse exec`
// sessions):
//
//   runtime.session.metadata            model_id, workspace_root
//   run.model.configured                model_id (per-run, latest wins)
//   session.name.changed                new_name (muse auto-names sessions;
//                                       there is no rename command)
//   runtime.session (payload.kind "run") wraps the run events:
//     started                           the user prompt
//     assistant_message_committed       assistant text (message_id, text)
//     assistant_tool_calls_committed    tool calls ({args, call_id, id, name})
//     tool_result_batch_committed       results ({text, tool_call_id, …})
//     reasoning_committed               encrypted_content — muse persists
//                                       reasoning encrypted, so it cannot be
//                                       rendered and is skipped
//
// Everything else (task lifecycle, context diagnostics, subagent control,
// cron, …) has no transcript rendering. Files are written by a live muse
// process, so the last line can be half-written; every line is parsed
// independently and unparseable lines are skipped — the same JSONL
// discipline as the pi store.
//
// This module is deliberately pure — no process state, no caching — so the
// harness owns the composition of file content with the live-streaming
// overlay (see lib/muse-run.js).

import { promises as fs } from "node:fs";
import path from "node:path";
import { xdgDataDir } from "./xdg.js";

// Tool output is joined into one part field; a hard cap keeps a single 1MB
// tool dump from ballooning every transcript response (same cap as the pi
// store).
const TOOL_OUTPUT_TRUNCATION = 2000;

// Muse resolves its data dir per the XDG spec: $XDG_DATA_HOME/muse, falling
// back to ~/.local/share/muse (lib/xdg.js). The bridge scans (and spawns)
// against the same resolution so the phone sees exactly the sessions the
// muse CLI sees.
export function defaultMuseSessionDir() {
  return path.join(xdgDataDir(), "muse", "sessions");
}

export function parseLine(line) {
  try {
    return JSON.parse(line.endsWith("\r") ? line.slice(0, -1) : line);
  } catch {
    return null; // a partial line while muse is mid-write, or a corrupt file
  }
}

// recorded_at is microseconds since the epoch; skiff's session objects carry
// milliseconds.
function timestampMs(record) {
  if (typeof record?.recorded_at !== "number") return null;
  return Math.floor(record.recorded_at / 1000);
}

// The session layout is exactly sessions/YYYY/MM/DD/<uuid>/session.jsonl.
// The fixed depth is what excludes subagent sessions — they nest deeper
// (<uuid>/subagent/<child>/session.jsonl) and are run internals, not
// conversations to list.
export async function listMuseSessionFiles(sessionDir) {
  const out = [];
  let years;
  try {
    years = await fs.readdir(sessionDir, { withFileTypes: true });
  } catch {
    return out; // a missing or unreadable session dir simply has no sessions
  }
  for (const year of years) {
    if (!year.isDirectory()) continue;
    for (const month of await readdirSafe(path.join(sessionDir, year.name))) {
      if (!month.isDirectory()) continue;
      for (const day of await readdirSafe(path.join(sessionDir, year.name, month.name))) {
        if (!day.isDirectory()) continue;
        for (const session of await readdirSafe(path.join(sessionDir, year.name, month.name, day.name))) {
          if (!session.isDirectory()) continue;
          const file = path.join(sessionDir, year.name, month.name, day.name, session.name, "session.jsonl");
          try {
            await fs.access(file);
            out.push(file);
          } catch {
            // a session dir muse created but has not written yet
          }
        }
      }
    }
  }
  return out;
}

async function readdirSafe(dir) {
  try {
    return await fs.readdir(dir, { withFileTypes: true });
  } catch {
    return [];
  }
}

// id -> absolute session.jsonl path, without reading any content. The id is
// the session dir's name (muse's session uuid), so resolution is a name walk
// over the dated layout.
export async function resolveMuseSessionFile(sessionDir, id) {
  for (const file of await listMuseSessionFiles(sessionDir)) {
    if (path.basename(path.dirname(file)) === id) return file;
  }
  return null;
}

export async function readMuseSessionFile(filePath) {
  let text;
  try {
    text = await fs.readFile(filePath, "utf8");
  } catch {
    return null;
  }
  const records = [];
  for (const line of text.split("\n")) {
    const record = parseLine(line);
    if (record && typeof record.payload_type === "string") records.push(record);
  }
  if (records.length === 0) return null;
  return { records };
}

// The run events, unwrapped. The durable file wraps them as
// payload_type "runtime.session" with payload.kind "run" and the event under
// payload.event; the same events appear unwrapped (payload_type
// "run.lifecycle.started" etc.) on `muse exec --json` stdout, but this
// module only ever reads the file.
function runEvent(record) {
  if (record.payload_type !== "runtime.session") return null;
  if (record.payload?.kind !== "run") return null;
  return record.payload.event ?? null;
}

// TUI-created sessions carry no session.name.changed record (only `muse
// exec` sessions get the automatic name); muse's own session index titles
// those by their first user prompt, and the list does the same so no real
// session ever reads "Untitled".
const TITLE_FALLBACK_LENGTH = 60;

export function buildMuseSessionObject(id, { records }) {
  let title = null;
  let firstPrompt = null;
  let directory = null;
  let model = null;
  for (const record of records) {
    if (record.payload_type === "session.name.changed" && typeof record.payload?.new_name === "string") {
      title = record.payload.new_name;
    } else if (firstPrompt === null && runEvent(record)?.kind === "started") {
      const prompt = runEvent(record).prompt;
      if (typeof prompt === "string" && prompt.trim() !== "") firstPrompt = prompt.trim();
    } else if (record.payload_type === "runtime.session.metadata") {
      const meta = record.payload?.record;
      if (typeof meta?.workspace_root === "string") directory = meta.workspace_root;
      if (typeof meta?.model_id === "string") model = meta.model_id;
    } else if (record.payload_type === "run.model.configured") {
      const configured = record.payload?.record;
      if (typeof configured?.model_id === "string") model = configured.model_id;
    }
  }
  if (title === null && firstPrompt !== null) {
    title = firstPrompt.length <= TITLE_FALLBACK_LENGTH ? firstPrompt : firstPrompt.slice(0, TITLE_FALLBACK_LENGTH) + "…";
  }
  const created = timestampMs(records[0]);
  const updated = timestampMs(records[records.length - 1]) ?? created;
  return {
    id,
    title,
    directory,
    time: { created, updated },
    model: model ? { id: model } : null,
  };
}

// Map a session's records to skiff's { info, parts } transcript shape.
export function mapMuseMessages(records) {
  const messages = [];
  // Tool parts are emitted by assistant_tool_calls_committed batches; the
  // matching tool_result_batch_committed then folds into them by call id, so
  // a tool's status/output lives on one part.
  const toolPartsByCallId = new Map();
  let model = null;
  for (const record of records) {
    if (record.payload_type === "runtime.session.metadata") {
      const meta = record.payload?.record;
      if (typeof meta?.model_id === "string") model = meta.model_id;
      continue;
    }
    if (record.payload_type === "run.model.configured") {
      const configured = record.payload?.record;
      if (typeof configured?.model_id === "string") model = configured.model_id;
      continue;
    }
    const event = runEvent(record);
    if (!event) continue;
    const created = timestampMs(record);
    switch (event.kind) {
      case "started":
        messages.push({
          info: { id: record.id, role: "user", time: { created } },
          parts: [{ type: "text", text: event.prompt ?? "", id: `${record.id}-p0` }],
        });
        break;
      case "assistant_message_committed":
        messages.push({
          info: { id: event.message_id ?? record.id, role: "assistant", agent: model, time: { created, completed: created } },
          parts: [{ type: "text", text: event.text ?? "", id: `${event.message_id ?? record.id}-p0` }],
        });
        break;
      case "assistant_tool_calls_committed": {
        const parts = [];
        for (const call of Array.isArray(event.tool_calls) ? event.tool_calls : []) {
          const part = { type: "tool", tool: call.name ?? "", id: call.call_id, state: { status: "running" } };
          if (call.call_id) toolPartsByCallId.set(call.call_id, part);
          parts.push(part);
        }
        messages.push({
          info: { id: event.message_id ?? record.id, role: "assistant", agent: model, time: { created, completed: created } },
          parts,
        });
        break;
      }
      case "tool_result_batch_committed": {
        const standalone = [];
        for (const result of Array.isArray(event.results) ? event.results : []) {
          const output = truncateOutput(result.text ?? "");
          const existing = toolPartsByCallId.get(result.tool_call_id);
          if (existing) {
            existing.state.status = "completed";
            existing.state.output = output;
          } else {
            // No matching call on record (a mid-write file): still surface
            // the outcome rather than silently dropping it — same policy as
            // the pi store's standalone toolResult fold.
            standalone.push({ type: "tool", tool: "", id: result.tool_call_id, state: { status: "completed", output } });
          }
        }
        if (standalone.length > 0) {
          messages.push({
            info: { id: record.id, role: "toolResult", time: { created } },
            parts: standalone,
          });
        }
        break;
      }
      default:
        break; // terminal, reasoning_committed (encrypted), diagnostics, …
    }
  }
  return messages;
}

function truncateOutput(text) {
  if (text.length <= TOOL_OUTPUT_TRUNCATION) return text;
  return text.slice(0, TOOL_OUTPUT_TRUNCATION) + "…";
}

export async function listMuseSessions(sessionDir) {
  const sessions = [];
  for (const file of await listMuseSessionFiles(sessionDir)) {
    const parsed = await readMuseSessionFile(file);
    if (!parsed) continue;
    sessions.push(buildMuseSessionObject(path.basename(path.dirname(file)), parsed));
  }
  sessions.sort((a, b) => (b.time?.updated ?? 0) - (a.time?.updated ?? 0));
  return sessions;
}
