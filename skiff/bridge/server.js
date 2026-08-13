// bridge/server.js
// Zero-dependency HTTP bridge: skiff's client speaks opencode's HTTP API, and
// this server answers it from pi's session files and live RPC processes.
//
// Composition model: the session store (lib/session-store.js) is a pure,
// file-based reader; the pool (lib/pi-rpc.js) owns the live pi processes and
// their streaming state. This file is the wiring: routes, auth, and the two
// places process state must be composed with file state —
//   1. the streaming overlay (an in-flight assistant message has no file
//      entry yet, so it is appended to the message list from the pool), and
//   2. newborn sessions (real pi creates NO file at new_session time; the
//      file appears only when the first message is persisted, so until then
//      the session object is served from the registered process).
//
// Security posture: every response is JSON, every error is {"error": ...},
// and prompt text and the password never reach logs or error surfaces
// (prompt payloads are parsed and forwarded, not echoed).

import http from "node:http";
import path from "node:path";
import os from "node:os";
import fs from "node:fs";
import { timingSafeEqual } from "node:crypto";
import { pathToFileURL } from "node:url";
import {
  defaultSessionDir,
  resolveSessionFile,
  readSessionFile,
  buildSessionObject,
  mapMessages,
  listSessions,
} from "./lib/session-store.js";
import { PiPool, PiError } from "./lib/pi-rpc.js";
import { createStreamRegistry } from "./lib/stream-registry.js";

const AUTH_USERNAME = "opencode";
const BODY_LIMIT_BYTES = 1 << 20; // 1 MiB — title and prompt bodies are tiny
const CREATE_READY_TIMEOUT_MS = 15000;
const CREATE_READY_POLL_MS = 100;

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

export function createBridge(config) {
  const ctx = {
    password: config.password ?? process.env.OPENCODE_SERVER_PASSWORD,
    host: config.host ?? process.env.PI_BRIDGE_HOST ?? "127.0.0.1",
    port: config.port ?? Number(process.env.PI_BRIDGE_PORT ?? 4120),
    sessionDir: config.sessionDir ?? defaultSessionDir(),
    defaultCwd: config.defaultCwd ?? expandHome(process.env.PI_BRIDGE_CWD ?? "~/code"),
    binary: resolvePiBinary(config.binary ?? process.env.PI_BINARY),
    maxProcesses: config.maxProcesses ?? Number(process.env.PI_BRIDGE_MAX_PROCESSES ?? 8),
  };
  // A bridge without auth would expose every session file on the LAN, so the
  // password is a hard boot requirement, not a default.
  if (!ctx.password || ctx.password === "") {
    throw new Error(
      "skiff-pi-bridge: OPENCODE_SERVER_PASSWORD must be set to the password skiff's opencode client uses."
    );
  }
  ctx.pool = new PiPool({
    binary: ctx.binary,
    sessionDir: ctx.sessionDir,
    sessionDirExplicit: config.sessionDirExplicit ?? envIsSet("PI_SESSION_DIR"),
    defaultCwd: ctx.defaultCwd,
    maxProcesses: ctx.maxProcesses,
  });
  // The stream registry is the push counterpart of the poll endpoints: it
  // holds per-session live state (transcript, overlay, orchestrator readout)
  // and fans it out over SSE. Wiring: the pool's processEvent hook feeds it
  // pi's live events, and isPinned keeps a subscribed session's process from
  // being LRU-evicted out from under its own stream.
  ctx.registry = createStreamRegistry({ pool: ctx.pool, sessionDir: ctx.sessionDir });
  ctx.pool.processEvent = (proc, event) => ctx.registry.onProcessEvent(proc, event);
  ctx.pool.isPinned = (sessionFile) => ctx.registry.hasSubscribers(sessionFile);

  const server = http.createServer(async (req, res) => {
    try {
      if (!authorized(req, ctx.password)) return sendJson(res, 401, { error: "unauthorized" });
      await handle(req, res, ctx);
    } catch (err) {
      if (err instanceof HttpError) return sendJson(res, err.status, { error: err.message });
      console.error("bridge internal error:", err);
      return sendJson(res, 500, { error: "internal error" });
    }
  });

  return {
    server,
    pool: ctx.pool,
    registry: ctx.registry,
    config: ctx,
    port() {
      return server.address()?.port ?? ctx.port;
    },
    listen() {
      return new Promise((resolve) => server.listen(ctx.port, ctx.host, resolve));
    },
    close() {
      ctx.registry.close();
      ctx.pool.shutdown();
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

// --- HTTP plumbing ----------------------------------------------------------

function expandHome(p) {
  if (p === "~") return os.homedir();
  if (p.startsWith("~/")) return path.join(os.homedir(), p.slice(2));
  return p;
}

function envIsSet(name) {
  const value = process.env[name];
  return typeof value === "string" && value.trim() !== "";
}

// The pi executable, resolved to an absolute path at boot.
//
// Why not just "pi"? The bridge runs under systemd user units (and launchd
// on the Mac), whose PATHs do not include ~/.local/bin — where pi is actually
// installed on the desktop. A command name that only resolves for interactive
// shells would fail intermittently on the first prompt, so resolution happens
// once at boot: PATH first, then the home-relative locations that match where
// pi installs itself. An explicit PI_BINARY is honored as-is when it carries a
// path; a bare name from PI_BINARY goes through the same search. Failing to
// find pi is a boot error, like a missing password — a bridge that cannot
// prompt is not a bridge.
function resolvePiBinary(explicit) {
  const candidates =
    explicit && explicit.trim() !== ""
      ? [explicit]
      : ["pi", path.join(os.homedir(), ".local", "bin", "pi"), path.join(os.homedir(), "bin", "pi")];
  for (const candidate of candidates) {
    if (candidate.includes("/")) {
      if (fs.existsSync(candidate)) return path.resolve(candidate);
      continue;
    }
    for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
      if (dir === "") continue;
      const resolved = path.join(dir, candidate);
      if (fs.existsSync(resolved)) return resolved;
    }
  }
  throw new Error(
    `skiff-pi-bridge: pi executable not found (tried: ${candidates.join(", ")}); set PI_BINARY to its absolute path`
  );
}

// Constant-time comparison: this is a localhost bridge, but password checks
// are exactly where timing side channels should not exist.
function safeEqual(a, b) {
  const ba = Buffer.from(a);
  const bb = Buffer.from(b);
  if (ba.length !== bb.length) return false;
  return timingSafeEqual(ba, bb);
}

function authorized(req, password) {
  const header = req.headers.authorization ?? "";
  const expected = "Basic " + Buffer.from(`${AUTH_USERNAME}:${password}`).toString("base64");
  return safeEqual(header, expected);
}

function sendJson(res, status, body) {
  if (status === 204) {
    res.writeHead(204);
    return res.end();
  }
  const payload = JSON.stringify(body);
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(payload);
}

// Body reader with a hard size cap; the request is drained to completion so a
// connection is never left half-read, then the 413 is raised at 'end'.
function readBody(req) {
  return new Promise((resolve, reject) => {
    let size = 0;
    let tooLarge = false;
    const chunks = [];
    req.on("data", (chunk) => {
      size += chunk.length;
      if (size > BODY_LIMIT_BYTES) {
        tooLarge = true;
        return;
      }
      chunks.push(chunk);
    });
    req.on("end", () => {
      if (tooLarge) return reject(new HttpError(413, "request body too large"));
      resolve(Buffer.concat(chunks).toString("utf8"));
    });
    req.on("error", reject);
  });
}

async function readJsonBody(req) {
  const raw = await readBody(req);
  if (raw === "") return null;
  try {
    return JSON.parse(raw);
  } catch {
    throw new HttpError(400, "invalid JSON body");
  }
}

// --- Session resolution -----------------------------------------------------

// A session target is either a parseable file (the normal case) or a live
// pool process whose file has not been written yet (newborn session). Both
// are first-class: skiff redirects straight to a freshly created session, so
// it must be readable before its first message persists it.
async function resolveSessionTarget(ctx, id) {
  const file = await resolveSessionFile(ctx.sessionDir, id);
  if (file) {
    const parsed = await readSessionFile(file);
    if (parsed) return { kind: "file", file, parsed };
    return null; // a file that is not a session is an unknown id
  }
  const proc = ctx.pool.getById(id);
  if (proc) return { kind: "proc", proc };
  return null;
}

// The session object for a newborn session (no file yet): id from the file
// path pi chose, title from the name the create flow set, directory from the
// process cwd, and the pool's bookkeeping for times. Model is null until the
// first message brings a model_change entry. Orchestrator mode is whatever
// the live process last recorded: pi buffers the toggle's custom entry in
// memory until the first assistant message persists the file, so the
// process's entry_appended record is the only one in the newborn window.
function newbornSessionObject(proc) {
  return {
    id: path.basename(proc.sessionFile, ".jsonl"),
    title: proc.sessionName ?? null,
    directory: proc.cwd,
    time: { created: proc.createdAt, updated: proc.lastActivity },
    model: null,
    orchestrator: { active: proc.lastOrchestratorActive ?? false },
  };
}

// Merge the live orchestrator readout into a session object. The mode tag's
// record comes from the session file (or the process in the newborn window);
// the plan/worker readout exists only in the live process — the extension
// publishes it fire-and-forget (setWidget/setStatus), and the bridge keeps
// the last publication per process. The widget/status fields are present
// only while the process holds one: cleared publications (mode off, idle)
// drop them, and a file session with no live process simply has no readout.
function withLiveOrchestrator(session, proc) {
  if (!proc) return session;
  if (proc.lastOrchestratorWidget === null && proc.lastOrchestratorStatus === null) return session;
  const orchestrator = { ...(session.orchestrator ?? {}) };
  if (proc.lastOrchestratorWidget !== null) orchestrator.widget = proc.lastOrchestratorWidget;
  if (proc.lastOrchestratorStatus !== null) orchestrator.status = proc.lastOrchestratorStatus;
  return { ...session, orchestrator };
}

// --- Routes -----------------------------------------------------------------

async function handle(req, res, ctx) {
  const { pathname } = new URL(req.url, "http://localhost");
  const parts = pathname.split("/").filter(Boolean);

  if (req.method === "GET" && parts.join("/") === "global/health") {
    return sendJson(res, 200, { status: "ok" });
  }
  if (req.method === "GET" && parts.length === 1 && parts[0] === "session") {
    return handleSessionList(res, ctx);
  }
  if (req.method === "POST" && parts.length === 1 && parts[0] === "session") {
    return handleSessionCreate(req, res, ctx);
  }
  // Must be matched before /session/{id} — "status" is not a session id, but
  // ordering keeps the intent explicit.
  if (req.method === "GET" && parts.length === 2 && parts[0] === "session" && parts[1] === "status") {
    return handleStatus(res, ctx);
  }
  if (req.method === "GET" && parts.length === 2 && parts[0] === "session") {
    return handleSessionShow(req, res, ctx, parts[1]);
  }
  if (req.method === "GET" && parts.length === 3 && parts[0] === "session" && parts[2] === "message") {
    return handleSessionMessages(res, ctx, parts[1]);
  }
  if (req.method === "GET" && parts.length === 3 && parts[0] === "session" && parts[2] === "stream") {
    return handleStream(res, ctx, parts[1]);
  }
  if (req.method === "POST" && parts.length === 3 && parts[0] === "session" && parts[2] === "prompt_async") {
    return handlePromptAsync(req, res, ctx, parts[1]);
  }
  if (req.method === "POST" && parts.length === 3 && parts[0] === "session" && parts[2] === "orchestrator") {
    return handleOrchestratorToggle(req, res, ctx, parts[1]);
  }
  if (req.method === "POST" && parts.length === 3 && parts[0] === "session" && parts[2] === "name") {
    return handleSessionRename(req, res, ctx, parts[1]);
  }
  if (req.method === "POST" && parts.length === 3 && parts[0] === "session" && parts[2] === "abort") {
    return handleAbort(req, res, ctx, parts[1]);
  }
  return sendJson(res, 404, { error: "not found" });
}

async function handleSessionList(res, ctx) {
  const sessions = await listSessions(ctx.sessionDir);
  // Newborn sessions have no file, so they are invisible to the file scan;
  // fold in pool processes whose file has not appeared yet. The file version
  // wins once it exists, so the same session can never be listed twice.
  const ids = new Set(sessions.map((s) => s.id));
  for (const proc of ctx.pool.processes.values()) {
    if (!proc.sessionFile) continue;
    const id = path.basename(proc.sessionFile, ".jsonl");
    if (!ids.has(id)) {
      sessions.push(newbornSessionObject(proc));
      ids.add(id);
    }
  }
  sessions.sort((a, b) => (b.time?.updated ?? 0) - (a.time?.updated ?? 0));
  return sendJson(res, 200, sessions);
}

async function handleSessionShow(req, res, ctx, id) {
  const target = await resolveSessionTarget(ctx, id);
  if (!target) return sendJson(res, 404, { error: `session ${id} not found` });
  if (target.kind === "proc") {
    return sendJson(res, 200, withLiveOrchestrator(newbornSessionObject(target.proc), target.proc));
  }
  const proc = ctx.pool.getByFile(target.file);
  return sendJson(res, 200, withLiveOrchestrator(buildSessionObject(target.file, target.parsed), proc));
}

async function handleSessionMessages(res, ctx, id) {
  const target = await resolveSessionTarget(ctx, id);
  if (!target) return sendJson(res, 404, { error: `session ${id} not found` });
  const messages = target.kind === "file" ? mapMessages(target.parsed.entries) : [];
  // Streaming overlay: while a live process for this session is mid-message,
  // append the in-flight assistant message after the file-parsed ones. This
  // is what lets the legacy poller show text growing (the stream endpoint
  // serves the same overlay through the registry).
  const proc = target.kind === "file" ? ctx.pool.getByFile(target.file) : target.proc;
  const pending = proc?.getPendingMessage();
  if (pending) messages.push(pending);
  return sendJson(res, 200, messages);
}

// GET /session/{id}/stream — the push counterpart of handleSessionMessages.
// Subscribes the response to the session's live state; the registry answers
// with SSE (snapshot first, then append/replace/remove/working/orchestrator
// events) until the client disconnects. The headers are written by the
// registry once the session resolves, so an unknown id still gets a normal
// 404.
async function handleStream(res, ctx, id) {
  const ok = await ctx.registry.subscribe(id, res);
  if (!ok) return sendJson(res, 404, { error: `session ${id} not found` });
}

async function handleSessionCreate(req, res, ctx) {
  const body = await readJsonBody(req);
  const title = typeof body?.title === "string" && body.title.trim() !== "" ? body.title : "New session";

  const proc = ctx.pool.spawnBare(ctx.defaultCwd);
  proc.createdAt = Date.now();
  try {
    await waitForReady(proc);
    const created = await proc.sendCommand("new_session", {}, { timeoutMs: CREATE_READY_TIMEOUT_MS });
    if (!created.success) throw new PiError(`pi rejected new_session: ${created.error ?? "unknown error"}`);
    const named = await proc.sendCommand("set_session_name", { name: title });
    if (!named.success) throw new PiError(`pi rejected set_session_name: ${named.error ?? "unknown error"}`);
    proc.sessionName = title;
    const state = await proc.sendCommand("get_state");
    const file = state.data?.sessionFile;
    if (!state.success || typeof file !== "string" || file === "") {
      throw new PiError("pi did not report a session file after new_session");
    }
    ctx.pool.register(proc, path.resolve(file));
    return sendJson(res, 201, { id: path.basename(file, ".jsonl") });
  } catch (err) {
    proc.kill();
    throw new HttpError(502, `create session failed: ${err.message}`);
  }
}

// Poll get_state until pi answers successfully. A bare RPC process needs a
// moment to boot its session manager; each attempt is bounded so a hung child
// cannot hold the create request past the overall deadline.
async function waitForReady(proc) {
  const deadline = Date.now() + CREATE_READY_TIMEOUT_MS;
  while (true) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new PiError("pi did not become ready within the timeout");
    let response;
    try {
      response = await proc.sendCommand("get_state", {}, { timeoutMs: Math.min(remaining, 5000) });
    } catch (err) {
      throw new PiError(`pi did not become ready: ${err.message}`);
    }
    if (response.success) return;
    await sleep(CREATE_READY_POLL_MS);
  }
}

// POST /session/{id}/prompt_async — skiff sends exactly one text part. Only
// text is forwarded (pi's prompt command takes a plain string); any other
// part type is rejected loudly rather than silently dropped, because a
// dropped image or file would be an invisible data loss.
async function handleOrchestratorToggle(req, res, ctx, id) {
  const target = await resolveSessionTarget(ctx, id);
  if (!target) return sendJson(res, 404, { error: `session ${id} not found` });

  const body = await readJsonBody(req);
  const on = typeof body?.on === "boolean" ? body.on : null;
  if (on === null) return sendJson(res, 400, { error: 'orchestrator toggle requires a boolean "on" field' });

  // The toggle drives the live pi process through its extension command
  // (prompt executes /orchestrator even while the agent streams); the
  // extension persists the mode into the session file, so the file and the
  // process stay consistent. Same lazy-spawn path as prompt/abort.
  let proc;
  if (target.kind === "file") {
    const cwd = typeof target.parsed.header.cwd === "string" ? target.parsed.header.cwd : ctx.defaultCwd;
    proc = await ctx.pool.ensure(target.file, cwd);
  } else {
    proc = target.proc;
  }

  try {
    const response = await proc.sendCommand("prompt", { message: on ? "/orchestrator on" : "/orchestrator off" });
    if (!response.success) throw new PiError(response.error ?? "pi rejected the orchestrator toggle");
    return sendJson(res, 200, { ok: true });
  } catch (err) {
    return sendJson(res, 502, { error: `orchestrator toggle failed: ${err.message}` });
  }
}

// POST /session/{id}/name — set the session display name. The rename drives
// the session's live pi process through its set_session_name command; pi
// persists the name as a session_info entry in the session file (the file
// reader serves it back on the next GET), and the process keeps it in memory
// for the newborn-session window when no file exists yet.
async function handleSessionRename(req, res, ctx, id) {
  const target = await resolveSessionTarget(ctx, id);
  if (!target) return sendJson(res, 404, { error: `session ${id} not found` });

  const body = await readJsonBody(req);
  const name = typeof body?.name === "string" ? body.name.trim() : "";
  if (name === "") return sendJson(res, 400, { error: 'rename requires a non-empty "name" field' });

  let proc;
  if (target.kind === "file") {
    const cwd = typeof target.parsed.header.cwd === "string" ? target.parsed.header.cwd : ctx.defaultCwd;
    proc = await ctx.pool.ensure(target.file, cwd);
  } else {
    proc = target.proc; // newborn: already registered with its own cwd
  }

  try {
    const response = await proc.sendCommand("set_session_name", { name });
    if (!response.success) throw new PiError(response.error ?? "pi rejected the rename");
    proc.sessionName = name;
    return sendJson(res, 200, { ok: true });
  } catch (err) {
    return sendJson(res, 502, { error: `rename failed: ${err.message}` });
  }
}

async function handlePromptAsync(req, res, ctx, id) {
  const target = await resolveSessionTarget(ctx, id);
  if (!target) return sendJson(res, 404, { error: `session ${id} not found` });

  const body = await readJsonBody(req);
  const parts = Array.isArray(body?.parts) ? body.parts : [];
  const unsupported = parts.find((part) => part?.type !== "text");
  if (unsupported) return sendJson(res, 400, { error: `unsupported part type: ${unsupported?.type ?? "unknown"}` });
  const text = parts.map((part) => part.text ?? "").join("\n");
  if (text === "") return sendJson(res, 400, { error: "prompt has no text" });

  let proc;
  if (target.kind === "file") {
    const cwd = typeof target.parsed.header.cwd === "string" ? target.parsed.header.cwd : ctx.defaultCwd;
    proc = await ctx.pool.ensure(target.file, cwd);
  } else {
    proc = target.proc; // newborn: already registered with its own cwd
  }

  try {
    const response = await proc.sendCommand("prompt", { message: text });
    if (!response.success) {
      // Never echo the prompt text; the pi error explains the rejection.
      throw new PiError(response.error ?? "pi rejected the prompt");
    }
    return sendJson(res, 200, { ok: true });
  } catch (err) {
    return sendJson(res, 502, { error: `prompt failed: ${err.message}` });
  }
}

async function handleAbort(req, res, ctx, id) {
  const target = await resolveSessionTarget(ctx, id);
  if (!target) return sendJson(res, 404, { error: `session ${id} not found` });

  let proc;
  if (target.kind === "file") {
    const cwd = typeof target.parsed.header.cwd === "string" ? target.parsed.header.cwd : ctx.defaultCwd;
    proc = await ctx.pool.ensure(target.file, cwd);
  } else {
    proc = target.proc;
  }

  try {
    const response = await proc.sendCommand("abort", {}, { timeoutMs: 10000 });
    if (!response.success) throw new PiError(response.error ?? "pi rejected abort");
    return sendJson(res, 204, null);
  } catch (err) {
    return sendJson(res, 502, { error: `abort failed: ${err.message}` });
  }
}

async function handleStatus(res, ctx) {
  // Busy is event-driven (agent_start .. agent_settled), so a process that is
  // still booting, or idle in the pool, is never reported busy.
  const statuses = {};
  for (const proc of ctx.pool.all()) {
    if (proc.isBusy() && proc.sessionFile) {
      statuses[path.basename(proc.sessionFile, ".jsonl")] = { type: "busy" };
    }
  }
  return sendJson(res, 200, statuses);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// --- CLI entry point --------------------------------------------------------

function isMain() {
  return Boolean(process.argv[1]) && import.meta.url === pathToFileURL(process.argv[1]).href;
}

export function main() {
  let bridge;
  try {
    bridge = createBridge({});
  } catch (err) {
    console.error(err.message);
    process.exit(1);
  }
  bridge.listen().then(() => {
    console.error(`skiff-pi-bridge listening on http://${bridge.config.host}:${bridge.port()}`);
  });

  let shuttingDown = false;
  const shutdown = () => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.error("skiff-pi-bridge shutting down");
    bridge.close().then(() => process.exit(0));
    // A stubborn keep-alive connection must not block the exit forever.
    setTimeout(() => process.exit(0), 2000).unref();
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
}

if (isMain()) main();
