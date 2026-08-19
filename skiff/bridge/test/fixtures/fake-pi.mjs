#!/usr/bin/env node
// fake-pi.mjs — a scripted stand-in for `pi --mode rpc` that drives the
// bridge's real code paths in tests without an LLM. It implements just enough
// of the protocol: get_state/new_session/set_session_name respond like pi,
// and a prompt streams a scripted assistant message (message_start -> text
// deltas with delays -> message_end -> agent_end -> agent_settled) while
// appending the finished entries to the session file.
//
// Two behaviors mirror real pi, both observed live before writing this:
// - new_session does NOT touch the disk: the session file is created lazily
//   when the first message is appended. Until then the bridge must serve the
//   session from process state (the pool), which is exactly what the
//   newborn-session path in server.js does.
// - responses and events share stdout and can interleave; the bridge
//   correlates by id, never by order.
//
// Env hooks for failure-path tests (the bridge passes its env through):
//   FAKE_PI_DELAY_MS           per-delta streaming delay (default 150)
//   FAKE_PI_PROMPT_ERROR=1     prompt command answers success:false
//   FAKE_PI_SET_NAME_ERROR=1   set_session_name answers success:false when
//                              the name contains "reject" (name-conditional
//                              so the create flow's own set_session_name
//                              can still succeed)
//   FAKE_PI_FAIL_NEW_SESSION=1 new_session answers success:false
//   FAKE_PI_EMIT_DIALOG=1      prompt emits a select dialog and waits for the
//                              bridge's extension_ui_response, echoing what it
//                              received to stderr (asserted by pi-rpc tests)
//   FAKE_PI_ARGV_FILE=<path>   append this process's argv (one JSON line) so
//                              tests can assert the exact spawn flags
//
// The file also guards against being executed as a test: `node --test`
// discovers any .mjs under a directory named "test", and running the fake as
// one must not block on stdin.

import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { StringDecoder } from "node:string_decoder";

// `pi --list-models` mode: the whitespace-padded table real pi prints —
// header line, then provider and model id as the first two columns. The
// bridge's listModels parses exactly this.
if (process.argv.includes("--list-models")) {
  if (process.env.FAKE_PI_LIST_MODELS_ERROR === "1") {
    process.stderr.write("fake: list-models refused (FAKE_PI_LIST_MODELS_ERROR)\n");
    process.exit(1);
  }
  process.stdout.write(
    [
      "provider      model              context  max-out  thinking  images",
      "deepseek      deepseek-v4-flash  1M       384K     yes       no",
      "deepseek      deepseek-v4-pro    1M       384K     yes       no",
      "muse-glimmer  muse-glimmer-30B   32.8K    8.2K     yes       yes",
      "",
    ].join("\n")
  );
  process.exit(0);
}

// Executed by the test runner without RPC args -> do nothing, exit clean.
if (!process.argv.includes("--mode")) process.exit(0);

const args = process.argv.slice(2);
if (process.env.FAKE_PI_ARGV_FILE) {
  fs.appendFileSync(process.env.FAKE_PI_ARGV_FILE, JSON.stringify(args) + "\n");
}
const sessionIndex = args.indexOf("--session");
const sessionDirIndex = args.indexOf("--session-dir");
const sessionFile = sessionIndex !== -1 ? args[sessionIndex + 1] : null;
// Session dir resolution mirrors real pi: --session-dir wins, then pi's own
// env override, then the platform default. The default-path tests therefore
// point the spawned fake at the bridge's scan dir via
// PI_CODING_AGENT_SESSION_DIR, exactly as pi would honor it.
const sessionDir =
  sessionDirIndex !== -1
    ? args[sessionDirIndex + 1]
    : process.env.PI_CODING_AGENT_SESSION_DIR || path.join(os.homedir(), ".pi", "agent", "sessions");
const cwd = process.cwd();
const delayMs = Number(process.env.FAKE_PI_DELAY_MS || 150);

let file = sessionFile; // the path pi reports via get_state
let lastEntryId = null;
let sessionName = null;
let aborted = false;
let dialogPending = false;
let dialogEchoed = false;

const nowIso = () => new Date().toISOString();
const nowMs = () => Date.now();
const tsName = () => nowIso().replace(/[:.]/g, "-");
const hex8 = () => crypto.randomBytes(4).toString("hex");
const uuid = () => crypto.randomUUID();

// The would-be session file for a fresh (create-flow) process. Like real pi,
// the file is NOT created here — only reported. It appears on the first
// append (see ensureFile).
if (!file) {
  file = path.join(sessionDir, `${tsName()}_${uuid()}.jsonl`);
} else if (fs.existsSync(file)) {
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    try {
      const entry = JSON.parse(line);
      if (entry && typeof entry.id === "string") lastEntryId = entry.id;
    } catch {
      // trailing partial line: ignore
    }
  }
}

// The lazy file creation, mirroring pi: the header is written first (with
// the process cwd), then ALL in-memory entries in append order — real pi
// buffers every pre-file entry (session_info from set_session_name, custom
// entries from extensions) and flushes them together when the first
// assistant message creates the file, so the parentId chains stay intact.
function ensureFile() {
  if (fs.existsSync(file)) return;
  const header = { type: "session", version: 3, id: uuid(), timestamp: nowIso(), cwd };
  fs.appendFileSync(file, JSON.stringify(header) + "\n");
  for (const entry of memoryEntries) {
    fs.appendFileSync(file, JSON.stringify(entry) + "\n");
    lastEntryId = entry.id;
  }
  memoryEntries.length = 0;
}

// Entries appended before the file exists. Real pi buffers them in memory
// until the first assistant message (appendCustomEntry -> _persist's
// hasAssistant gate, verified live: a toggle on a newborn session never
// reaches disk). Custom entries emit entry_appended in both phases — the
// event the bridge tracks so the newborn window still serves the mode.
const memoryEntries = [];
let hasAssistant = false;

function appendEntry(entry) {
  if (!hasAssistant && entry.type !== "message") {
    memoryEntries.push(entry);
    lastEntryId = entry.id;
    if (entry.type === "custom") {
      send({ type: "entry_appended", entry });
    }
    return;
  }
  ensureFile();
  fs.appendFileSync(file, JSON.stringify(entry) + "\n");
  lastEntryId = entry.id;
  if (entry.type === "message" && entry.message?.role === "assistant") {
    hasAssistant = true;
  }
  if (entry.type === "custom") {
    send({ type: "entry_appended", entry });
  }
}

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

function respond(id, command, success, data, error) {
  const response = { type: "response", command, success };
  if (id !== undefined) response.id = id;
  if (data !== undefined) response.data = data;
  if (error !== undefined) response.error = error;
  send(response);
}

function emitAgentEnd() {
  send({ type: "agent_end", messages: [], willRetry: false });
}

function emitAgentSettled() {
  send({ type: "agent_settled" });
}

function chunkMessage(text, n) {
  const chars = [...text];
  const size = Math.max(1, Math.ceil(chars.length / n));
  const chunks = [];
  for (let i = 0; i < chars.length; i += size) chunks.push(chars.slice(i, i + size).join(""));
  return chunks.length > 0 ? chunks : [""];
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function handlePrompt(cmd) {
  if (process.env.FAKE_PI_PROMPT_ERROR === "1") {
    return respond(cmd.id, "prompt", false, undefined, "fake: prompt refused (FAKE_PI_PROMPT_ERROR)");
  }
  respond(cmd.id, "prompt", true); // accepted; events stream after

  // The orchestrator extension's commands run synchronously and persist a
  // custom entry via pi.appendEntry; real pi never streams a message for
  // them. Mirror that: record the toggle on disk and return without the
  // scripted assistant turn, so the bridge's session object picks the mode
  // up from the file exactly as it would in production.
  const text = typeof cmd.message === "string" ? cmd.message : "";
  const toggle = text.match(/^\/orchestrator (on|off)$/);
  if (toggle) {
    appendEntry({
      type: "custom",
      id: hex8(),
      parentId: lastEntryId,
      timestamp: nowIso(),
      customType: "orchestrator-mode",
      data: { active: toggle[1] === "on", at: nowMs() },
    });
    // The real extension follows every toggle with updateWidget: a
    // fire-and-forget setWidget/setStatus publication carrying the live
    // state lines, or a clear when the mode went off. Mirror it so the
    // bridge's capture path is exercised end-to-end.
    if (toggle[1] === "on") {
      send({
        type: "extension_ui_request",
        id: uuid(),
        method: "setWidget",
        widgetKey: "orchestrator",
        widgetLines: ["◉ orchestrator ◉ planned — fake plan · 1s", "  · Step one", "  · Step two"],
      });
      send({
        type: "extension_ui_request",
        id: uuid(),
        method: "setStatus",
        statusKey: "orchestrator",
        statusText: "orchestrator: planned · 0/2 steps done",
      });
    } else {
      send({ type: "extension_ui_request", id: uuid(), method: "setWidget", widgetKey: "orchestrator" });
      send({ type: "extension_ui_request", id: uuid(), method: "setStatus", statusKey: "orchestrator" });
    }
    return;
  }

  if (process.env.FAKE_PI_EMIT_DIALOG === "1") {
    const dialogId = uuid();
    dialogPending = true;
    send({ type: "extension_ui_request", id: dialogId, method: "select", title: "fake dialog", options: ["a", "b"] });
    const deadline = Date.now() + 3000;
    while (dialogPending && Date.now() < deadline) await sleep(10);
    if (dialogPending) {
      process.stderr.write(`FAKE_DIALOG_TIMEOUT ${dialogId}\n`);
      dialogPending = false;
    } else {
      process.stderr.write(`FAKE_DIALOG_RESPONSE ${dialogId} cancelled=${dialogEchoed}\n`);
    }
    if (aborted) return;
  }

  // The first message persists the session file: the header (and the
  // session_info entry carrying the name set via set_session_name, when any)
  // land first, so this message's parentId chains onto the session_info
  // entry — exactly how real pi's append order works (verified live).
  ensureFile();
  const userEntry = {
    type: "message",
    id: hex8(),
    parentId: lastEntryId,
    timestamp: nowIso(),
    message: { role: "user", content: [{ type: "text", text }], timestamp: nowMs() },
  };
  appendEntry(userEntry);

  send({ type: "agent_start" });
  send({ type: "message_start", message: { role: "assistant", content: [], model: "fake-model" } });
  send({ type: "message_update", assistantMessageEvent: { type: "text_start", contentIndex: 0 } });

  let emitted = "";
  for (const chunk of chunkMessage(text, 3)) {
    if (aborted) return;
    emitted += chunk;
    send({ type: "message_update", assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: chunk } });
    await sleep(delayMs);
  }
  if (aborted) return;

  send({ type: "message_update", assistantMessageEvent: { type: "text_end", contentIndex: 0, content: emitted } });
  const content = [{ type: "text", text: emitted }];
  send({
    type: "message_end",
    message: { role: "assistant", content, model: "fake-model", stopReason: "stop", timestamp: nowMs() },
  });
  appendEntry({
    type: "message",
    id: hex8(),
    parentId: lastEntryId,
    timestamp: nowIso(),
    message: { role: "assistant", content, model: "fake-model", stopReason: "stop", timestamp: nowMs() },
  });
  emitAgentEnd();
  emitAgentSettled();
}

function handleLine(line) {
  let cmd;
  try {
    cmd = JSON.parse(line);
  } catch {
    return;
  }
  if (cmd.type === "extension_ui_response") {
    if (dialogPending && cmd.id) {
      dialogEchoed = cmd.cancelled === true;
      dialogPending = false;
    }
    return;
  }
  switch (cmd.type) {
    case "get_state":
      respond(cmd.id, "get_state", true, {
        model: null,
        thinkingLevel: "medium",
        isStreaming: false,
        isCompacting: false,
        sessionFile: file,
        sessionName: sessionName ?? undefined,
        messageCount: 0,
        pendingMessageCount: 0,
      });
      break;
    case "new_session":
      if (process.env.FAKE_PI_FAIL_NEW_SESSION === "1") {
        return respond(cmd.id, "new_session", false, undefined, "fake: new_session refused (FAKE_PI_FAIL_NEW_SESSION)");
      }
      respond(cmd.id, "new_session", true, { cancelled: false });
      break;
    case "set_session_name":
      // The failure hook is name-conditional: the create flow also calls
      // set_session_name (with the initial title), and that call must keep
      // succeeding for the created session to exist at all. Only a rename
      // to a name containing "reject" trips the failure.
      if (process.env.FAKE_PI_SET_NAME_ERROR === "1" && String(cmd.name ?? "").includes("reject")) {
        return respond(cmd.id, "set_session_name", false, undefined, "fake: set_session_name refused (FAKE_PI_SET_NAME_ERROR)");
      }
      sessionName = typeof cmd.name === "string" ? cmd.name : null;
      // Mirror real pi: set_session_name appends a session_info entry — to
      // the file on a persisted session, into the pre-file buffer on a
      // newborn (flushed with the first assistant message) — so the bridge's
      // file reader serves the new title on the next GET.
      appendEntry({
        type: "session_info",
        id: hex8(),
        parentId: lastEntryId,
        timestamp: nowIso(),
        name: sessionName,
      });
      respond(cmd.id, "set_session_name", true);
      break;
    case "set_model": {
      const known = {
        "deepseek/deepseek-v4-flash": true,
        "deepseek/deepseek-v4-pro": true,
        "muse-glimmer/muse-glimmer-30B": true,
      };
      if (!known[`${cmd.provider}/${cmd.modelId}`]) {
        return respond(cmd.id, "set_model", false, undefined, `Model not found: ${cmd.provider}/${cmd.modelId}`);
      }
      // Mirror real pi: the switch appends a model_change entry (file or
      // pre-file buffer), so the session object reflects it immediately.
      appendEntry({
        type: "model_change",
        id: hex8(),
        parentId: lastEntryId,
        timestamp: nowIso(),
        provider: cmd.provider,
        modelId: cmd.modelId,
      });
      respond(cmd.id, "set_model", true, { id: cmd.modelId, provider: cmd.provider });
      break;
    }
    case "prompt":
      handlePrompt(cmd); // async; intentionally not awaited
      break;
    case "abort":
      aborted = true;
      respond(cmd.id, "abort", true);
      emitAgentEnd();
      break;
    default:
      respond(cmd.id, cmd.type, false, undefined, `fake: unsupported command ${cmd.type}`);
  }
}

// Strict JSONL framing on stdin, mirroring the bridge's reader.
const decoder = new StringDecoder("utf8");
let buffer = "";
process.stdin.on("data", (chunk) => {
  buffer += decoder.write(chunk);
  let nl;
  while ((nl = buffer.indexOf("\n")) !== -1) {
    let line = buffer.slice(0, nl);
    buffer = buffer.slice(nl + 1);
    if (line.endsWith("\r")) line = line.slice(0, -1);
    handleLine(line);
  }
});
process.stdin.on("end", () => process.exit(0));
