// bridge/server.js
// Zero-dependency HTTP bridge: skiff's client speaks one JSON API, and this
// server answers it from whichever coding-agent harness owns the session —
// pi (lib/pi-harness.js), Meta's Muse Code (lib/muse-harness.js), or a
// headless opencode serve (lib/opencode-harness.js).
//
// Composition model: this file is routing, auth, and body handling only.
// Session ids on the wire are harness-qualified ("pi:…", "muse:…",
// "opencode:…" — lib/ids.js); each harness serves its own sessions behind
// one interface (list/get/messages/create/prompt/abort/… plus a stream
// driver for the generic registry in lib/stream-registry.js), and declares
// capabilities — rename and the orchestrator toggle exist only where the
// harness supports them, and unsupported operations are rejected loudly, not
// silently dropped. The /change family (DW-002's change object — rounds,
// annotations, card binding) is served by lib/changes.js the same way.
//
// Security posture: every response is JSON, every error is {"error": ...},
// and prompt text and the password never reach logs or error surfaces
// (prompt payloads are parsed and forwarded, not echoed).

import http from "node:http";
import os from "node:os";
import path from "node:path";
import { timingSafeEqual } from "node:crypto";
import { pathToFileURL } from "node:url";
import { HttpError } from "./lib/errors.js";
import { formatSessionId, parseSessionId } from "./lib/ids.js";
import { createStreamRegistry } from "./lib/stream-registry.js";
import { createPiHarness } from "./lib/pi-harness.js";
import { createMuseHarness } from "./lib/muse-harness.js";
import { createOpencodeHarness } from "./lib/opencode-harness.js";
import { createChanges, createUnavailableChanges } from "./lib/changes.js";

const AUTH_USERNAME = "skiff";
const BODY_LIMIT_BYTES = 1 << 20; // 1 MiB — title and prompt bodies are tiny

export function createBridge(config) {
  const ctx = {
    password: config.password ?? process.env.SKIFF_BRIDGE_PASSWORD,
    host: config.host ?? process.env.SKIFF_BRIDGE_HOST ?? "127.0.0.1",
    port: config.port ?? Number(process.env.SKIFF_BRIDGE_PORT ?? 4120),
    defaultCwd: config.defaultCwd ?? expandHome(process.env.SKIFF_BRIDGE_CWD ?? "~/code"),
  };
  // A bridge without auth would expose every session file on the LAN, so the
  // password is a hard boot requirement, not a default.
  if (!ctx.password || ctx.password === "") {
    throw new Error(
      "skiff-bridge: SKIFF_BRIDGE_PASSWORD must be set to the password skiff's bridge client uses."
    );
  }
  ctx.registry = createStreamRegistry(config.stream ?? {});
  ctx.harnesses = new Map();
  for (const [name, create] of [
    ["pi", () => createPiHarness(config.pi ?? {}, { registry: ctx.registry, defaultCwd: ctx.defaultCwd })],
    ["muse", () => createMuseHarness(config.muse ?? {}, { defaultCwd: ctx.defaultCwd })],
    ["opencode", () => createOpencodeHarness(config.opencode ?? {})],
  ]) {
    // A host missing one harness (no muse on the Mac, say) must not lose the
    // other two: the harness degrades to a stub that lists nothing, reports
    // itself in the session list's errors map, and answers every operation
    // with the boot failure — visible, never silent.
    try {
      const harness = create();
      ctx.harnesses.set(harness.name, harness);
    } catch (err) {
      console.error(`skiff-bridge: ${name} harness unavailable: ${err.message}`);
      ctx.harnesses.set(name, createUnavailableHarness(name, err.message));
    }
  }
  // The change subsystem (DW-002 §4 — rounds, annotations, card binding)
  // degrades the same way a harness does: a host without jj keeps its
  // sessions and loses /change with a visible 502, never silently.
  try {
    ctx.changes = createChanges(config.changes ?? {}, { defaultCwd: ctx.defaultCwd });
  } catch (err) {
    console.error(`skiff-bridge: change subsystem unavailable: ${err.message}`);
    ctx.changes = createUnavailableChanges(err.message);
  }

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
    harnesses: ctx.harnesses,
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
      for (const harness of ctx.harnesses.values()) harness.shutdown();
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

// A harness that failed to construct (its binary is not installed on this
// host). Sessions from it cannot exist, so gets answer null (404) and the
// list reports the reason; anything that would drive it fails with the boot
// error.
function createUnavailableHarness(name, message) {
  const fail = () => {
    throw new HttpError(502, `${name} harness unavailable: ${message}`);
  };
  return {
    name,
    capabilities: { rename: false, orchestrator: false, model: false },
    async listSessions() {
      throw new Error(message);
    },
    async getSession() {
      return null;
    },
    async getMessages() {
      return null;
    },
    createSession: fail,
    promptAsync: fail,
    abort: fail,
    busyLocalIds: () => [],
    async resolveStreamDriver() {
      return null;
    },
    shutdown() {},
  };
}

// --- HTTP plumbing ----------------------------------------------------------

function expandHome(p) {
  if (p === "~") return os.homedir();
  if (p.startsWith("~/")) return path.join(os.homedir(), p.slice(2));
  return p;
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

// --- Dispatch ---------------------------------------------------------------

// A wire session id resolves to its harness and the harness's own local id;
// an unknown prefix (or an unprefixed id) is an unknown session, full stop.
function resolveHarness(ctx, id) {
  const parsed = parseSessionId(id);
  const harness = parsed ? ctx.harnesses.get(parsed.harness) : null;
  if (!harness) throw new HttpError(404, `session ${id} not found`);
  return { harness, localId: parsed.localId };
}

// Tag a harness-local session object for the wire: prefixed id, the harness
// name, and its capabilities, so skiff renders exactly the controls the
// session's harness supports.
function tagSession(harness, session) {
  return {
    ...session,
    id: formatSessionId(harness.name, session.id),
    harness: harness.name,
    capabilities: harness.capabilities,
  };
}

// --- Routes -----------------------------------------------------------------

async function handle(req, res, ctx) {
  const { pathname } = new URL(req.url, "http://localhost");
  const parts = pathname.split("/").filter(Boolean);

  if (req.method === "GET" && parts.join("/") === "global/health") {
    return sendJson(res, 200, { status: "ok" });
  }
  if (req.method === "GET" && parts.length === 3 && parts[0] === "harness" && parts[2] === "models") {
    return handleHarnessModels(res, ctx, parts[1]);
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
    return handleSessionShow(res, ctx, parts[1]);
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
  if (req.method === "POST" && parts.length === 3 && parts[0] === "session" && parts[2] === "model") {
    return handleSessionModel(req, res, ctx, parts[1]);
  }
  if (req.method === "POST" && parts.length === 3 && parts[0] === "session" && parts[2] === "abort") {
    return handleAbort(res, ctx, parts[1]);
  }
  if (parts[0] === "change") {
    return handleChange(req, res, ctx, parts);
  }
  return sendJson(res, 404, { error: "not found" });
}

// --- Changes (DW-002 §4: one card, one change, additive rounds) -------------
//
// /change/{repo}/{card} — the card number is the only identifier the user
// ever sees; the repo name scopes it to one jj repository under the bridge's
// repos root. lib/changes.js owns all semantics; these routes only shape
// HTTP. Approve (rebase-and-push, the Fizzy comment) is step 03 and has no
// route yet — submit and reopen are the whole lifecycle surface for now.
async function handleChange(req, res, ctx, parts) {
  if (req.method === "GET" && parts.length === 1) {
    return sendJson(res, 200, { changes: await ctx.changes.list() });
  }
  if (req.method === "POST" && parts.length === 1) {
    const body = await readJsonBody(req);
    const change = await ctx.changes.create(body?.repo, body?.card);
    return sendJson(res, 201, change);
  }
  if (req.method === "GET" && parts.length === 3) {
    return sendJson(res, 200, await ctx.changes.get(parts[1], parts[2]));
  }
  if (req.method === "GET" && parts.length === 4 && parts[3] === "diff") {
    return sendJson(res, 200, { diff: await ctx.changes.diff(parts[1], parts[2]) });
  }
  if (req.method === "GET" && parts.length === 5 && parts[3] === "diff") {
    return sendJson(res, 200, { diff: await ctx.changes.diff(parts[1], parts[2], parts[4]) });
  }
  if (req.method === "POST" && parts.length === 4 && parts[3] === "round") {
    const body = await readJsonBody(req);
    const round = await ctx.changes.addRound(parts[1], parts[2], {
      author: body?.author,
      changeId: body?.changeId,
      note: body?.note,
    });
    return sendJson(res, 201, round);
  }
  if (req.method === "POST" && parts.length === 4 && parts[3] === "annotation") {
    const body = await readJsonBody(req);
    const annotation = await ctx.changes.addAnnotation(parts[1], parts[2], {
      round: body?.round,
      path: body?.path,
      line: body?.line,
      side: body?.side,
      text: body?.text,
    });
    return sendJson(res, 201, annotation);
  }
  if (req.method === "POST" && parts.length === 4 && parts[3] === "submit") {
    return sendJson(res, 200, await ctx.changes.transition(parts[1], parts[2], "in_review"));
  }
  if (req.method === "POST" && parts.length === 4 && parts[3] === "reopen") {
    return sendJson(res, 200, await ctx.changes.transition(parts[1], parts[2], "working"));
  }
  return sendJson(res, 404, { error: "not found" });
}

// GET /session — every harness's sessions in one list, newest first. A
// harness that cannot answer (opencode serve down, an unreadable session
// dir) is reported in `errors` by name rather than silently vanishing —
// skiff shows the gap instead of pretending those sessions do not exist.
async function handleSessionList(res, ctx) {
  const sessions = [];
  const errors = {};
  await Promise.all(
    [...ctx.harnesses.values()].map(async (harness) => {
      try {
        for (const session of await harness.listSessions()) {
          sessions.push(tagSession(harness, session));
        }
      } catch (err) {
        errors[harness.name] = err.message;
      }
    })
  );
  sessions.sort((a, b) => (b.time?.updated ?? 0) - (a.time?.updated ?? 0));
  return sendJson(res, 200, { sessions, errors });
}

async function handleSessionCreate(req, res, ctx) {
  const body = await readJsonBody(req);
  const harness = ctx.harnesses.get(body?.harness);
  if (!harness) {
    return sendJson(res, 400, {
      error: `create requires a "harness" field naming one of: ${[...ctx.harnesses.keys()].join(", ")}`,
    });
  }
  const title = typeof body?.title === "string" && body.title.trim() !== "" ? body.title : "New session";
  const localId = await harness.createSession({ title });
  return sendJson(res, 201, { id: formatSessionId(harness.name, localId) });
}

async function handleSessionShow(res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  const session = await harness.getSession(localId);
  if (!session) return sendJson(res, 404, { error: `session ${id} not found` });
  return sendJson(res, 200, tagSession(harness, session));
}

async function handleSessionMessages(res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  const messages = await harness.getMessages(localId);
  if (!messages) return sendJson(res, 404, { error: `session ${id} not found` });
  return sendJson(res, 200, messages);
}

// GET /session/{id}/stream — the push counterpart of handleSessionMessages.
// Subscribes the response to the session's live state; the registry answers
// with SSE (snapshot first, then append/replace/remove/working/orchestrator
// events) until the client disconnects. The headers are written by the
// registry once the session resolves, so an unknown id still gets a normal
// 404.
async function handleStream(res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  const ok = await ctx.registry.subscribe(id, res, () => harness.resolveStreamDriver(localId));
  if (!ok) return sendJson(res, 404, { error: `session ${id} not found` });
}

// POST /session/{id}/prompt_async — skiff sends exactly one text part. Only
// text is forwarded (every harness's prompt surface takes a plain string);
// any other part type is rejected loudly rather than silently dropped,
// because a dropped image or file would be an invisible data loss.
async function handlePromptAsync(req, res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  const body = await readJsonBody(req);
  const parts = Array.isArray(body?.parts) ? body.parts : [];
  const unsupported = parts.find((part) => part?.type !== "text");
  if (unsupported) return sendJson(res, 400, { error: `unsupported part type: ${unsupported?.type ?? "unknown"}` });
  const text = parts.map((part) => part.text ?? "").join("\n");
  if (text === "") return sendJson(res, 400, { error: "prompt has no text" });

  await harness.promptAsync(localId, text);
  return sendJson(res, 200, { ok: true });
}

// POST /session/{id}/orchestrator — pi's orchestrator extension. Harnesses
// without the capability reject the toggle; skiff never renders the control
// for them, so reaching this is a client bug worth surfacing.
async function handleOrchestratorToggle(req, res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  if (!harness.capabilities.orchestrator) {
    return sendJson(res, 400, { error: `${harness.name} sessions have no orchestrator` });
  }
  const body = await readJsonBody(req);
  const on = typeof body?.on === "boolean" ? body.on : null;
  if (on === null) return sendJson(res, 400, { error: 'orchestrator toggle requires a boolean "on" field' });

  await harness.orchestrator(localId, on);
  return sendJson(res, 200, { ok: true });
}

// POST /session/{id}/name — set the session display name, where the harness
// has a rename surface (pi persists a session_info entry; opencode PATCHes
// the session; muse names its sessions itself and rejects here).
async function handleSessionRename(req, res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  if (!harness.capabilities.rename) {
    return sendJson(res, 400, { error: `${harness.name} sessions cannot be renamed` });
  }
  const body = await readJsonBody(req);
  const name = typeof body?.name === "string" ? body.name.trim() : "";
  if (name === "") return sendJson(res, 400, { error: 'rename requires a non-empty "name" field' });

  await harness.rename(localId, name);
  return sendJson(res, 200, { ok: true });
}

// GET /harness/{name}/models — the models sessions of this harness can
// switch to. Only harnesses with the model capability (pi) have a list.
async function handleHarnessModels(res, ctx, name) {
  const harness = ctx.harnesses.get(name);
  if (!harness) return sendJson(res, 404, { error: `unknown harness ${name}` });
  if (!harness.capabilities.model) {
    return sendJson(res, 400, { error: `${name} sessions have no model switch` });
  }
  return sendJson(res, 200, await harness.listModels());
}

// POST /session/{id}/model — switch the session's model, where the harness
// supports it (pi's set_model; pi also persists the choice as its default).
async function handleSessionModel(req, res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  if (!harness.capabilities.model) {
    return sendJson(res, 400, { error: `${harness.name} sessions have no model switch` });
  }
  const body = await readJsonBody(req);
  const provider = typeof body?.provider === "string" ? body.provider.trim() : "";
  const modelId = typeof body?.id === "string" ? body.id.trim() : "";
  if (provider === "" || modelId === "") {
    return sendJson(res, 400, { error: 'model switch requires "provider" and "id" fields' });
  }
  await harness.setModel(localId, { provider, id: modelId });
  return sendJson(res, 200, { ok: true });
}

async function handleAbort(res, ctx, id) {
  const { harness, localId } = resolveHarness(ctx, id);
  await harness.abort(localId);
  return sendJson(res, 204, null);
}

// GET /session/status — the busy map for sessions with a live run the bridge
// itself is driving (pi: agent_start .. agent_settled; muse: an exec child
// in flight). opencode owns its runs server-side and contributes nothing
// here; its working state surfaces through the stream instead.
async function handleStatus(res, ctx) {
  const statuses = {};
  for (const harness of ctx.harnesses.values()) {
    for (const localId of harness.busyLocalIds()) {
      statuses[formatSessionId(harness.name, localId)] = { type: "busy" };
    }
  }
  return sendJson(res, 200, statuses);
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
    console.error(`skiff-bridge listening on http://${bridge.config.host}:${bridge.port()}`);
  });

  let shuttingDown = false;
  const shutdown = () => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.error("skiff-bridge shutting down");
    bridge.close().then(() => process.exit(0));
    // A stubborn keep-alive connection must not block the exit forever.
    setTimeout(() => process.exit(0), 2000).unref();
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
}

if (isMain()) main();
