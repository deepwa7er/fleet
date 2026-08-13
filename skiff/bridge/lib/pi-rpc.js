// bridge/lib/pi-rpc.js
// Zero-dependency client for pi's RPC protocol (`pi --mode rpc`) plus the
// process pool that keeps one live pi per session.
//
// Protocol facts this module leans on (verified against a real pi, and
// documented in pi's rpc.md):
// - Framing is strict JSONL: LF is the only record delimiter. Node's readline
//   also splits on U+2028/U+2029, which are legal inside JSON strings, so the
//   splitter is hand-rolled.
// - Responses and events share stdout; commands carry an `id` and the
//   matching response echoes it. Responses can arrive OUT OF ORDER (a real pi
//   answered a later get_state before an earlier new_session), so correlation
//   is by id, never by arrival order.
// - pi emits extension_ui_request events before any command (fire-and-forget
//   setWidget/setStatus from the orchestrator extension — the live plan and
//   worker readout — plus dialog methods). Dialog methods
//   (select/confirm/input/editor) block the agent until an
//   extension_ui_response arrives, so the bridge auto-cancels them — there is
//   no human on the other end of this bridge, and a deadlocked prompt would
//   wedge every stream subscriber forever.

import { spawn } from "node:child_process";
import { StringDecoder } from "node:string_decoder";
import path from "node:path";
import { mapMessageEntry } from "./session-store.js";

// The overlay entry id for the in-flight assistant message. It is a fixed
// placeholder because the real entry id is only assigned when pi persists the
// message; the stream registry and the message endpoint resolve it to the
// real entry when the file lands (the registry emits a replace at the
// overlay's index; the poll-free fallback — newest assistant message without
// time.completed — keeps working while it streams).
const PENDING_MESSAGE_ID = "<pending>";

const DIALOG_METHODS = new Set(["select", "confirm", "input", "editor"]);

// Read a stream as JSONL records: split on "\n" only, strip one trailing "\r",
// emit each line as a string. Chunks are decoded with StringDecoder so a
// multi-byte UTF-8 sequence split across chunks cannot corrupt a record.
export function createJsonlReader(stream, onLine, onEnd = () => {}, onError = () => {}) {
  const decoder = new StringDecoder("utf8");
  let buffer = "";
  stream.on("data", (chunk) => {
    buffer += decoder.write(chunk);
    let nl;
    while ((nl = buffer.indexOf("\n")) !== -1) {
      let line = buffer.slice(0, nl);
      buffer = buffer.slice(nl + 1);
      if (line.endsWith("\r")) line = line.slice(0, -1);
      onLine(line);
    }
  });
  stream.on("end", () => {
    buffer += decoder.end();
    if (buffer.length > 0) onLine(buffer.endsWith("\r") ? buffer.slice(0, -1) : buffer);
    onEnd();
  });
  stream.on("error", onError);
  return stream;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export class PiError extends Error {}

export class PiProcess {
  // sessionFile is null for the bare process spawned by the create flow
  // before pi reports the new session's file; onExit fires once when the
  // child dies (any reason), letting the pool drop its entry. processEvent
  // fires for every parsed stdout event after this process's own handling
  // (so overlay state is current when observers read it) — the stream
  // registry's hook into the live pi stream.
  constructor({ binary, sessionFile = null, cwd, sessionDir, passSessionDir = false, onExit = () => {}, processEvent = null }) {
    this.binary = binary;
    this.sessionFile = sessionFile;
    this.cwd = cwd;
    this.sessionDir = sessionDir;
    this.passSessionDir = passSessionDir;
    this.onExit = onExit;
    this.processEvent = processEvent;
    this.child = null;
    this.exited = false;
    this.nextCommandId = 0;
    this.pending = new Map(); // command id -> { resolve, reject, timer }
    this.busy = false; // agent_start .. agent_settled
    this.pendingMessage = null; // in-flight assistant message assembly
    this.sessionName = null; // set via set_session_name (create flow and rename)
    this.lastOrchestratorActive = null; // latest orchestrator-mode entry_appended
    // Latest fire-and-forget publications from the orchestrator extension
    // (setWidget/setStatus for its key): the live plan/worker readout. The
    // last publication per process is all a poller needs; null means the
    // extension cleared it (mode off, idle).
    this.lastOrchestratorWidget = null; // string[] | null
    this.lastOrchestratorStatus = null; // string | null
    this.lastActivity = Date.now(); // LRU eviction key
    this.stderrTail = "";
    this._exitReason = null;
  }

  start() {
    if (this.child) return;
    // --session-dir is passed only when the bridge's session dir was
    // explicitly overridden. pi's native default is the same directory the
    // bridge scans by default, but organizes files per-cwd (--<cwd>--/
    // buckets) — the layout the pi CLI itself reads and writes. Forcing
    // --session-dir always would make pi write sessions flat into the dir
    // root, invisible to the CLI's project-scoped listings, and silently
    // split the phone's sessions from the CLI's. The explicit-override case
    // accepts the flat layout deliberately: a custom dir has no native pi
    // layout to preserve, and scan and writes must stay in the same place.
    const args = ["--mode", "rpc"];
    if (this.passSessionDir) args.push("--session-dir", this.sessionDir);
    if (this.sessionFile) args.push("--session", this.sessionFile);
    const child = spawn(this.binary, args, { cwd: this.cwd, stdio: ["pipe", "pipe", "pipe"] });
    this.child = child;
    this.exited = false;

    createJsonlReader(child.stdout, (line) => this._onLine(line));
    // stderr is kept as a bounded tail purely for diagnostics on pi failures.
    // Prompt text and the password never appear here (prompts travel stdin).
    const decoder = new StringDecoder("utf8");
    child.stderr.on("data", (chunk) => {
      this.stderrTail = (this.stderrTail + decoder.write(chunk)).slice(-4096);
    });
    child.stderr.on("end", () => {
      this.stderrTail += decoder.end();
    });
    // EPIPE on a dying child is expected; the exit event drives teardown.
    child.stdin.on("error", () => {});
    child.on("error", (err) => this._handleExit(`spawn failed: ${err.code ?? err.message}`));
    child.on("exit", (code, signal) => {
      const reason = code === 0 ? "clean exit" : `exit code ${code}${signal ? ` (${signal})` : ""}`;
      this._handleExit(reason);
    });
  }

  _handleExit(reason) {
    if (this.exited) return;
    this.exited = true;
    this._exitReason = reason;
    for (const { reject, timer } of this.pending.values()) {
      clearTimeout(timer);
      reject(new PiError(`pi process ${reason}`));
    }
    this.pending.clear();
    this.pendingMessage = null;
    this.busy = false;
    this.onExit(this);
  }

  kill() {
    if (this.child && !this.exited) this.child.kill("SIGTERM");
  }

  isBusy() {
    return this.busy;
  }

  getExitReason() {
    return this._exitReason;
  }

  getStderrTail() {
    return this.stderrTail;
  }

  // Send one command and resolve with the correlated response. Rejects when
  // the process dies or the response never arrives (the response timeout is a
  // bound, not a promise that the command failed — pi's prompt response can
  // legitimately trail its events).
  sendCommand(type, fields = {}, { timeoutMs = 30000 } = {}) {
    if (this.exited || !this.child) {
      return Promise.reject(new PiError("pi process is not running"));
    }
    const id = String(++this.nextCommandId);
    const command = { type, id, ...fields };
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new PiError(`pi did not respond to ${type} within ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.lastActivity = Date.now();
      try {
        this.child.stdin.write(JSON.stringify(command) + "\n");
      } catch (err) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(new PiError(`failed to write ${type} command: ${err.message}`));
      }
    });
  }

  _write(obj) {
    if (!this.child || this.exited) return;
    try {
      this.child.stdin.write(JSON.stringify(obj) + "\n");
    } catch {
      // the exit event owns teardown
    }
  }

  _onLine(line) {
    let event;
    try {
      event = JSON.parse(line);
    } catch {
      return; // an unparseable line from pi is not worth surfacing
    }
    if (event.type === "response") this._onResponse(event);
    else {
      switch (event.type) {
        case "message_start":
          this._onMessageStart(event);
          break;
        case "message_update":
          this._onMessageUpdate(event);
          break;
        case "message_end":
          // The entry lands on disk with message_end (verified against real
          // pi), so the overlay's job is done — the file reader takes over.
          this.pendingMessage = null;
          break;
        case "agent_start":
          this.busy = true;
          this.lastActivity = Date.now();
          break;
        case "agent_end":
          if (!event.willRetry) this.busy = false;
          // An assistant message that never reached message_end (aborted or
          // errored run) will never be persisted; keep showing it would freeze
          // the transcript on content that can never stabilize.
          this.pendingMessage = null;
          break;
        case "agent_settled":
          this.busy = false;
          this.pendingMessage = null;
          break;
        case "entry_appended": {
          // The extension API's appendEntry emits this for every custom entry.
          // The orchestrator mode's shared record is such an entry, and before
          // the first assistant message pi buffers it in memory — the file is
          // not flushed until an assistant message exists (verified live), so
          // the process's last seen value is the only record then.
          // newbornSessionObject serves it for the newborn window.
          const entry = event.entry;
          if (entry?.customType === "orchestrator-mode" && typeof entry.data?.active === "boolean") {
            this.lastOrchestratorActive = entry.data.active;
          }
          break;
        }
        case "extension_ui_request":
          this._onExtensionUiRequest(event);
          break;
        default:
          break; // tool_execution_*, queue_update, compaction_*, turn_*, etc.
      }
    }
    if (this.processEvent) this.processEvent(this, event);
  }

  _onResponse(event) {
    const pending = this.pending.get(event.id);
    if (!pending) return; // a response for an already-timed-out command
    clearTimeout(pending.timer);
    this.pending.delete(event.id);
    pending.resolve(event);
  }

  // Only assistant messages stream into the overlay; user/toolResult messages
  // are persisted directly and the file reader surfaces them.
  _onMessageStart(event) {
    const message = event.message ?? {};
    if (message.role !== "assistant") return;
    this.pendingMessage = {
      blocks: new Map(), // contentIndex -> content block under construction
      model: typeof message.model === "string" ? message.model : null,
    };
  }

  // Assemble the in-flight assistant message from deltas. Content blocks are
  // keyed by contentIndex (never by arrival order — toolcalls and text can
  // interleave); toolcall_end carries the authoritative completed call and
  // replaces whatever the deltas accumulated.
  _onMessageUpdate(event) {
    const assembly = this.pendingMessage;
    if (!assembly) return;
    const delta = event.assistantMessageEvent;
    if (!delta) return;
    const index = delta.contentIndex ?? 0;
    switch (delta.type) {
      case "text_start":
        this._block(assembly, index, { type: "text", text: "" });
        break;
      case "text_delta":
        this._block(assembly, index, { type: "text", text: "" }).text += delta.delta ?? "";
        break;
      case "text_end": {
        const block = this._block(assembly, index, { type: "text", text: "" });
        if (typeof delta.content === "string") block.text = delta.content; // authoritative
        break;
      }
      case "thinking_start":
        this._block(assembly, index, { type: "thinking", thinking: "" });
        break;
      case "thinking_delta":
        // The file format's thinking block field is "thinking", not "text" —
        // the assembly must match so mapMessageEntry renders it as reasoning.
        this._block(assembly, index, { type: "thinking", thinking: "" }).thinking += delta.delta ?? "";
        break;
      case "thinking_end": {
        const block = this._block(assembly, index, { type: "thinking", thinking: "" });
        if (typeof delta.content === "string") block.thinking = delta.content;
        break;
      }
      case "toolcall_start": {
        const seed = delta.toolCall ?? {};
        this._block(assembly, index, { type: "toolCall", ...seed });
        break;
      }
      case "toolcall_delta": {
        const block = this._block(assembly, index, { type: "toolCall", id: "", name: "", arguments: "" });
        if (typeof delta.delta === "string") block.arguments = (block.arguments ?? "") + delta.delta;
        break;
      }
      case "toolcall_end":
        if (delta.toolCall) assembly.blocks.set(index, { ...delta.toolCall });
        break;
      default:
        break;
    }
  }

  _block(assembly, index, template) {
    if (!assembly.blocks.has(index)) assembly.blocks.set(index, template);
    return assembly.blocks.get(index);
  }

  // The orchestrator extension publishes its live state fire-and-forget via
  // setWidget/setStatus (see pi's rpc.md extension UI protocol): the widget
  // is the plan/worker readout, the status its compact summary. The bridge
  // captures the last publication per process so skiff's poller can serve
  // it; every other fire-and-forget method stays a display hint and is
  // ignored, and dialog methods are auto-cancelled below.
  _onExtensionUiRequest(event) {
    if (event.method === "setWidget" && event.widgetKey === "orchestrator") {
      this.lastOrchestratorWidget = Array.isArray(event.widgetLines) ? event.widgetLines : null;
      return;
    }
    if (event.method === "setStatus" && event.statusKey === "orchestrator") {
      this.lastOrchestratorStatus = typeof event.statusText === "string" ? event.statusText : null;
      return;
    }
    if (!DIALOG_METHODS.has(event.method)) return; // fire-and-forget: display hints, ignore
    this._write({ type: "extension_ui_response", id: event.id, cancelled: true });
  }

  // The overlay message: file-parsed messages plus this, assembled from the
  // deltas, with no time.completed so skiff's working indicator fires. Blocks
  // are emitted in contentIndex order.
  getPendingMessage() {
    const assembly = this.pendingMessage;
    if (!assembly) return null;
    const blocks = [...assembly.blocks.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([, block]) => block);
    return mapMessageEntry(
      {
        type: "message",
        id: PENDING_MESSAGE_ID,
        message: { role: "assistant", content: blocks, model: assembly.model },
        timestamp: Date.now(),
      },
      { streaming: true }
    );
  }
}

export class PiPool {
  // processEvent: called for every parsed process event after the process's
  // own handling (see PiProcess); the stream registry's live-event hook.
  // isPinned: a per-sessionFile predicate consulted by LRU eviction — a
  // session with open stream subscribers must not be evicted out from under
  // its own stream (the registry sets both via createBridge's wiring).
  constructor({ binary, sessionDir, sessionDirExplicit = false, defaultCwd, maxProcesses, processEvent = null, isPinned = () => false }) {
    this.binary = binary;
    this.sessionDir = sessionDir;
    this.sessionDirExplicit = sessionDirExplicit;
    this.defaultCwd = defaultCwd;
    this.maxProcesses = maxProcesses;
    this.processEvent = processEvent;
    this.isPinned = isPinned;
    this.processes = new Map(); // sessionFile -> PiProcess (registered, named sessions)
    this.bare = new Set(); // processes mid-create (no session file yet)
  }

  _makeProcess(sessionFile, cwd) {
    const proc = new PiProcess({
      binary: this.binary,
      sessionFile,
      cwd,
      sessionDir: this.sessionDir,
      passSessionDir: this.sessionDirExplicit,
      processEvent: this.processEvent,
      onExit: (p) => this._onProcessExit(p),
    });
    proc.start();
    return proc;
  }

  // Spawn the bare process for the create flow. It is not registered by file
  // until pi reports the new session's file (register()).
  spawnBare(cwd) {
    const proc = this._makeProcess(null, cwd);
    this.bare.add(proc);
    return proc;
  }

  // Move a create-flow process into the keyed pool once it has a session
  // file, then enforce the LRU cap.
  register(proc, sessionFile) {
    this.bare.delete(proc);
    proc.sessionFile = sessionFile;
    this.processes.set(sessionFile, proc);
    this._evict();
  }

  // Return the live process for a session file, spawning (and registering)
  // one on demand. This is the lazy-spawn path for prompt_async/abort.
  async ensure(sessionFile, cwd) {
    const existing = this.processes.get(sessionFile);
    if (existing) return existing;
    const proc = this._makeProcess(sessionFile, cwd);
    this.processes.set(sessionFile, proc);
    this._evict();
    return proc;
  }

  getByFile(sessionFile) {
    return this.processes.get(sessionFile) ?? null;
  }

  // Resolve by session id (file basename without .jsonl) — needed for the
  // newborn-session window when the file does not exist on disk yet.
  getById(id) {
    for (const proc of this.processes.values()) {
      if (proc.sessionFile && path.basename(proc.sessionFile, ".jsonl") === id) return proc;
    }
    return null;
  }

  all() {
    return [...this.processes.values(), ...this.bare];
  }

  _onProcessExit(proc) {
    this.bare.delete(proc);
    if (proc.sessionFile && this.processes.get(proc.sessionFile) === proc) {
      this.processes.delete(proc.sessionFile);
    }
    // Synthetic event so observers (the stream registry) can react to the
    // process vanishing — an in-flight overlay can no longer resolve through
    // deltas once its process is gone.
    if (this.processEvent) this.processEvent(proc, { type: "process_exit" });
  }

  // LRU eviction: kill the oldest idle process until at or under the cap.
  // Busy processes are never evicted (killing a live run would corrupt the
  // session), and pinned processes (open stream subscribers) are never
  // evicted either (their stream depends on the live event hook), so a busy
  // or watched pool may transiently exceed the cap.
  _evict() {
    if (this.processes.size <= this.maxProcesses) return;
    const idle = [...this.processes.values()]
      .filter((proc) => !proc.isBusy() && !this.isPinned(proc.sessionFile))
      .sort((a, b) => a.lastActivity - b.lastActivity);
    while (this.processes.size > this.maxProcesses && idle.length > 0) {
      const victim = idle.shift();
      this.processes.delete(victim.sessionFile);
      victim.kill();
    }
  }

  shutdown() {
    for (const proc of this.all()) proc.kill();
    this.processes.clear();
    this.bare.clear();
  }
}
