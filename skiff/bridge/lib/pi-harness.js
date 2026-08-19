// bridge/lib/pi-harness.js
// The pi harness: skiff sessions backed by `pi --mode rpc` processes and
// pi's v3 JSONL session files. This module owns everything pi-specific that
// used to live in server.js and the stream registry — binary resolution, the
// process pool, session-target resolution (file vs newborn process), the
// create flow's three-command dance, and the live-event switch that feeds
// the generic stream registry.
//
// Composition model (unchanged from the single-harness bridge): the session
// store (lib/session-store.js) is a pure, file-based reader; the pool
// (lib/pi-rpc.js) owns the live pi processes and their streaming state. The
// harness is the wiring, including the two places process state must be
// composed with file state —
//   1. the streaming overlay (an in-flight assistant message has no file
//      entry yet, so it is appended to the message list from the pool), and
//   2. newborn sessions (real pi creates NO file at new_session time; the
//      file appears only when the first message is persisted, so until then
//      the session object is served from the registered process).

import path from "node:path";
import { execFile } from "node:child_process";
import {
  defaultSessionDir,
  resolveSessionFile,
  readSessionFile,
  buildSessionObject,
  mapMessages,
  listSessions,
  parseLine,
  orchestratorActiveFromEntries,
} from "./session-store.js";
import { PiPool, PiError } from "./pi-rpc.js";
import { createFileTail } from "./file-tail.js";
import { HttpError } from "./errors.js";
import { formatSessionId } from "./ids.js";
import { resolveBinary } from "./resolve-binary.js";

const CREATE_READY_TIMEOUT_MS = 15000;
const CREATE_READY_POLL_MS = 100;
// `pi --list-models` is authoritative (it resolves auth, custom providers,
// and the built-in catalog exactly as pi will at prompt time) but costs a
// process spawn, so the parsed list is cached briefly. Models change rarely;
// a stale minute is invisible and self-heals.
const LIST_MODELS_CACHE_MS = 60_000;
const LIST_MODELS_TIMEOUT_MS = 15_000;
// After agent_settled, a still-unresolved overlay gets one last deferred
// file scan before being removed: pi emits message_end and persists the
// entry concurrently, so the synchronous scan at settled can race the write
// by a few milliseconds. One retry closes that window; if the entry still
// never lands, the overlay is removed (it never persisted) and a later
// append renders it normally if pi catches up.
const SETTLE_SCAN_DELAY_MS = 300;

function basenameId(file) {
  return path.basename(file, ".jsonl");
}

function envIsSet(name) {
  const value = process.env[name];
  return typeof value === "string" && value.trim() !== "";
}

export function createPiHarness(config, { registry, defaultCwd }) {
  const sessionDir = config.sessionDir ?? defaultSessionDir();
  const pool = new PiPool({
    binary: resolveBinary("pi", config.binary ?? process.env.PI_BINARY),
    sessionDir,
    sessionDirExplicit: config.sessionDirExplicit ?? envIsSet("PI_SESSION_DIR"),
    defaultCwd,
    maxProcesses: config.maxProcesses ?? Number(process.env.PI_BRIDGE_MAX_PROCESSES ?? 8),
  });
  const drivers = new Map(); // local session id -> live stream driver
  pool.processEvent = (proc, event) => {
    if (!proc.sessionFile) return; // a bare create-flow process has no session identity
    drivers.get(basenameId(proc.sessionFile))?.onProcessEvent(proc, event);
  };
  // A session with open stream subscribers must not be LRU-evicted out from
  // under its own stream (its overlay resolves through the live event hook).
  pool.isPinned = (sessionFile) =>
    Boolean(sessionFile) && registry.hasSubscribers(formatSessionId("pi", basenameId(sessionFile)));

  // A session target is either a parseable file (the normal case) or a live
  // pool process whose file has not been written yet (newborn session). Both
  // are first-class: skiff redirects straight to a freshly created session,
  // so it must be readable before its first message persists it.
  async function resolveTarget(localId) {
    const file = await resolveSessionFile(sessionDir, localId);
    if (file) {
      const parsed = await readSessionFile(file);
      if (parsed) return { kind: "file", file, parsed };
      return null; // a file that is not a session is an unknown id
    }
    const proc = pool.getById(localId);
    if (proc) return { kind: "proc", proc };
    return null;
  }

  // The session object for a newborn session (no file yet): id from the file
  // path pi chose, title from the name the create flow set, directory from
  // the process cwd, and the pool's bookkeeping for times. Model is null
  // until the first message brings a model_change entry. Orchestrator mode is
  // whatever the live process last recorded: pi buffers the toggle's custom
  // entry in memory until the first assistant message persists the file, so
  // the process's entry_appended record is the only one in the newborn
  // window.
  function newbornSessionObject(proc) {
    return {
      id: basenameId(proc.sessionFile),
      title: proc.sessionName ?? null,
      directory: proc.cwd,
      time: { created: proc.createdAt, updated: proc.lastActivity },
      model: null,
      orchestrator: { active: proc.lastOrchestratorActive ?? false },
    };
  }

  // Merge the live orchestrator readout into a session object. The mode
  // tag's record comes from the session file (or the process in the newborn
  // window); the plan/worker readout exists only in the live process — the
  // extension publishes it fire-and-forget (setWidget/setStatus), and the
  // bridge keeps the last publication per process. The widget/status fields
  // are present only while the process holds one: cleared publications (mode
  // off, idle) drop them, and a file session with no live process simply has
  // no readout.
  function withLiveOrchestrator(session, proc) {
    if (!proc) return session;
    if (proc.lastOrchestratorWidget === null && proc.lastOrchestratorStatus === null) return session;
    const orchestrator = { ...(session.orchestrator ?? {}) };
    if (proc.lastOrchestratorWidget !== null) orchestrator.widget = proc.lastOrchestratorWidget;
    if (proc.lastOrchestratorStatus !== null) orchestrator.status = proc.lastOrchestratorStatus;
    return { ...session, orchestrator };
  }

  // The live process for a session target, spawning one on demand. This is
  // the lazy-spawn path for prompt/abort/rename/orchestrator.
  async function ensureProcess(target) {
    if (target.kind === "proc") return target.proc; // newborn: registered with its own cwd
    const cwd = typeof target.parsed.header.cwd === "string" ? target.parsed.header.cwd : defaultCwd;
    return pool.ensure(target.file, cwd);
  }

  // Poll get_state until pi answers successfully. A bare RPC process needs a
  // moment to boot its session manager; each attempt is bounded so a hung
  // child cannot hold the create request past the overall deadline.
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
      await new Promise((resolve) => setTimeout(resolve, CREATE_READY_POLL_MS));
    }
  }

  const binary = pool.binary;
  let modelsCache = null; // { at, models }

  return {
    name: "pi",
    capabilities: { rename: true, orchestrator: true, model: true },
    pool, // exposed for tests (process introspection) and shutdown wiring

    // The models a session can switch to, from `pi --list-models`: a header
    // line then one whitespace-padded row per model, first two columns
    // provider and model id (neither can contain spaces). pi only lists
    // models whose provider actually resolves credentials, so everything
    // returned here is usable.
    async listModels() {
      if (modelsCache && Date.now() - modelsCache.at < LIST_MODELS_CACHE_MS) return modelsCache.models;
      const stdout = await new Promise((resolve, reject) => {
        execFile(binary, ["--list-models"], { timeout: LIST_MODELS_TIMEOUT_MS }, (err, out) => {
          if (err) reject(new HttpError(502, `pi --list-models failed: ${err.message}`));
          else resolve(out);
        });
      });
      const models = [];
      for (const line of stdout.split("\n").slice(1)) {
        const [provider, id] = line.trim().split(/\s+/);
        if (provider && id) models.push({ provider, id });
      }
      if (models.length === 0) throw new HttpError(502, "pi --list-models returned no models");
      modelsCache = { at: Date.now(), models };
      return models;
    },

    // Switch the session's model. pi appends a model_change entry to the
    // session file (the session object reflects it immediately) and — pi's
    // own semantics — persists the choice as the new default for future
    // sessions. An unknown model is the client's mistake (a stale picker),
    // not a pi failure.
    async setModel(localId, { provider, id }) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session pi:${localId} not found`);
      const proc = await ensureProcess(target);
      let response;
      try {
        response = await proc.sendCommand("set_model", { provider, modelId: id });
      } catch (err) {
        throw new HttpError(502, `model switch failed: ${err.message}`);
      }
      if (!response.success) {
        const error = response.error ?? "pi rejected the model switch";
        throw new HttpError(error.startsWith("Model not found") ? 400 : 502, `model switch failed: ${error}`);
      }
    },

    async listSessions() {
      const sessions = await listSessions(sessionDir);
      // Newborn sessions have no file, so they are invisible to the file
      // scan; fold in pool processes whose file has not appeared yet. The
      // file version wins once it exists, so the same session can never be
      // listed twice.
      const ids = new Set(sessions.map((s) => s.id));
      for (const proc of pool.processes.values()) {
        if (!proc.sessionFile) continue;
        const id = basenameId(proc.sessionFile);
        if (!ids.has(id)) {
          sessions.push(newbornSessionObject(proc));
          ids.add(id);
        }
      }
      return sessions;
    },

    async getSession(localId) {
      const target = await resolveTarget(localId);
      if (!target) return null;
      if (target.kind === "proc") return withLiveOrchestrator(newbornSessionObject(target.proc), target.proc);
      const proc = pool.getByFile(target.file);
      return withLiveOrchestrator(buildSessionObject(target.file, target.parsed), proc);
    },

    async getMessages(localId) {
      const target = await resolveTarget(localId);
      if (!target) return null;
      const messages = target.kind === "file" ? mapMessages(target.parsed.entries) : [];
      // Streaming overlay: while a live process for this session is
      // mid-message, append the in-flight assistant message after the
      // file-parsed ones (the stream endpoint serves the same overlay
      // through the registry).
      const proc = target.kind === "file" ? pool.getByFile(target.file) : target.proc;
      const pending = proc?.getPendingMessage();
      if (pending) messages.push(pending);
      return messages;
    },

    async createSession({ title }) {
      const proc = pool.spawnBare(defaultCwd);
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
        pool.register(proc, path.resolve(file));
        return basenameId(file);
      } catch (err) {
        proc.kill();
        throw new HttpError(502, `create session failed: ${err.message}`);
      }
    },

    async promptAsync(localId, text) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session pi:${localId} not found`);
      const proc = await ensureProcess(target);
      try {
        const response = await proc.sendCommand("prompt", { message: text });
        // Never echo the prompt text; the pi error explains the rejection.
        if (!response.success) throw new PiError(response.error ?? "pi rejected the prompt");
      } catch (err) {
        throw new HttpError(502, `prompt failed: ${err.message}`);
      }
    },

    async abort(localId) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session pi:${localId} not found`);
      const proc = await ensureProcess(target);
      try {
        const response = await proc.sendCommand("abort", {}, { timeoutMs: 10000 });
        if (!response.success) throw new PiError(response.error ?? "pi rejected abort");
      } catch (err) {
        throw new HttpError(502, `abort failed: ${err.message}`);
      }
    },

    // Set the session display name. The rename drives the session's live pi
    // process through its set_session_name command; pi persists the name as
    // a session_info entry in the session file (the file reader serves it
    // back on the next GET), and the process keeps it in memory for the
    // newborn-session window when no file exists yet.
    async rename(localId, name) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session pi:${localId} not found`);
      const proc = await ensureProcess(target);
      try {
        const response = await proc.sendCommand("set_session_name", { name });
        if (!response.success) throw new PiError(response.error ?? "pi rejected the rename");
        proc.sessionName = name;
      } catch (err) {
        throw new HttpError(502, `rename failed: ${err.message}`);
      }
    },

    // The toggle drives the live pi process through its extension command
    // (prompt executes /orchestrator even while the agent streams); the
    // extension persists the mode into the session file, so the file and the
    // process stay consistent. Same lazy-spawn path as prompt/abort.
    async orchestrator(localId, on) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session pi:${localId} not found`);
      const proc = await ensureProcess(target);
      try {
        const response = await proc.sendCommand("prompt", { message: on ? "/orchestrator on" : "/orchestrator off" });
        if (!response.success) throw new PiError(response.error ?? "pi rejected the orchestrator toggle");
      } catch (err) {
        throw new HttpError(502, `orchestrator toggle failed: ${err.message}`);
      }
    },

    // Busy is event-driven (agent_start .. agent_settled), so a process that
    // is still booting, or idle in the pool, is never reported busy.
    busyLocalIds() {
      const ids = [];
      for (const proc of pool.all()) {
        if (proc.isBusy() && proc.sessionFile) ids.push(basenameId(proc.sessionFile));
      }
      return ids;
    },

    // The stream driver: the pi side of the generic registry. Two sources
    // feed the session state, mirroring the poll endpoints' composition:
    //
    //  1. The file (via lib/file-tail.js). pi persists entries append-only
    //     and rewrites on compaction; every delivery is mapped with the
    //     session store (same leaf-branch and toolResult-folding rules the
    //     HTTP layer uses) and handed to the registry to diff.
    //  2. The live pi process (via the pool's processEvent hook). Only
    //     assistant message events matter: the overlay opens at
    //     message_start, grows with message_update, and message_end marks it
    //     "resolving".
    //
    // Resolution rule (deterministic, verified against real pi): pi persists
    // the assistant entry exactly at message_end, and nothing else can
    // append while the overlay is live, so the next file entry landing at
    // the overlay's index is the same message — resolvesOverlay accepts any
    // assistant entry. message_end and the watcher can race, so
    // agent_end/agent_settled force a synchronous file scan (plus one
    // deferred retry) before concluding the overlay never persisted.
    async resolveStreamDriver(localId) {
      const file = await resolveSessionFile(sessionDir, localId);
      const proc = file ? pool.getByFile(file) : pool.getById(localId);
      if (!file && !proc) return null;
      const sessionFile = file ?? proc.sessionFile;

      return (handle) => {
        const driver = {
          proc,
          entries: [],
          // The FIRST delivery always rebuilds into a snapshot, however it
          // arrives: the unborn-file path delivers the initial read through
          // the watcher (initial: false), and a diff against an empty
          // previous transcript would replay history as appends.
          reset: true,
          phase: null, // "live" | "resolving" — the overlay's lifecycle
          settleTimer: null,
          closed: false,

          overlayEntry() {
            return this.proc?.getPendingMessage() ?? null;
          },

          resolvesOverlay(entry) {
            // Nothing else can append while a pi overlay streams, so any
            // assistant entry at the overlay's index is its persistence; a
            // non-assistant entry is a pre-run entry the watcher delivered
            // late, which the registry kicks the overlay for.
            return entry.info.role === "assistant";
          },

          onOverlayResolved() {
            this.phase = null;
          },

          onLines(lines, initial) {
            for (const line of lines) {
              const entry = parseLine(line);
              if (!entry) continue;
              this.entries.push(entry);
              if (entry.type === "custom" && entry.customType === "orchestrator-mode") {
                // File-only sessions (no live process) still see mode
                // toggles; the live path also delivers them via
                // entry_appended, so both sources converge.
                handle.setOrchestrator({ active: entry.data?.active === true }, { broadcast: false });
              }
            }
            if (initial || this.reset) {
              this.reset = false;
              handle.setOrchestrator({ active: orchestratorActiveFromEntries(this.entries) }, { broadcast: false });
              handle.deliverTranscript(mapMessages(this.entries), { snapshot: true });
              return;
            }
            handle.deliverTranscript(mapMessages(this.entries));
          },

          onProcessEvent(proc, event) {
            this.proc = proc;
            switch (event.type) {
              case "message_start":
                if (event.message?.role === "assistant") {
                  this.phase = "live";
                  handle.overlayOpen();
                }
                break;
              case "message_update":
                if (this.phase === "live" && handle.hasOverlay()) handle.overlayChanged();
                break;
              case "message_end":
                if (handle.hasOverlay()) {
                  // The entry lands on disk with message_end; the file side
                  // resolves the overlay when it scans it.
                  this.phase = "resolving";
                  handle.overlayResolving();
                }
                break;
              case "agent_start":
                handle.setWorking(true);
                break;
              case "agent_end":
                if (!handle.hasOverlay()) break;
                if (this.phase === "live") {
                  // The run ended without message_end: the message never
                  // persisted.
                  this.phase = null;
                  handle.overlayDrop();
                } else {
                  // Resolving: the persisted entry raced the watcher; scan
                  // the file to settle it now rather than leaving the
                  // bubble indeterminate.
                  this.tail.scan();
                }
                break;
              case "agent_settled":
                handle.setWorking(false);
                if (handle.hasOverlay()) {
                  this.tail.scan(); // the entry should be on disk; the watcher may lag the pipe
                  if (handle.hasOverlay() && !this.settleTimer) {
                    // The write can still race the scan (pi persists
                    // concurrently with message_end); one deferred scan
                    // before concluding.
                    this.settleTimer = setTimeout(() => {
                      this.settleTimer = null;
                      if (this.closed || !handle.hasOverlay()) return;
                      this.tail.scan();
                      if (handle.hasOverlay()) {
                        this.phase = null;
                        handle.overlayDrop();
                      }
                    }, SETTLE_SCAN_DELAY_MS);
                    this.settleTimer.unref?.();
                  }
                }
                break;
              case "entry_appended":
                if (event.entry?.customType === "orchestrator-mode") {
                  // setWidget/setStatus publications ride along so the
                  // readout stays whole.
                  handle.setOrchestrator({
                    active: event.entry.data?.active === true,
                    widget: proc.lastOrchestratorWidget,
                    status: proc.lastOrchestratorStatus,
                  });
                }
                break;
              case "extension_ui_request":
                // setWidget/setStatus for the orchestrator key are the live
                // plan and worker readout; the process captured them before
                // this hook ran.
                if (event.method === "setWidget" || event.method === "setStatus") {
                  handle.setOrchestrator({
                    widget: proc.lastOrchestratorWidget,
                    status: proc.lastOrchestratorStatus,
                  });
                }
                break;
              case "process_exit":
                // The process died mid-run (crash or shutdown). The overlay
                // can no longer resolve through deltas; if the message
                // persists, the file side appends it normally, so dropping
                // the overlay is safe. The run is over either way.
                if (handle.hasOverlay()) {
                  this.phase = null;
                  handle.overlayDrop();
                }
                handle.setWorking(false);
                break;
              default:
                break; // responses, tool_execution_*, queue_update, compaction_*, …
            }
          },

          close() {
            this.closed = true;
            if (this.settleTimer) clearTimeout(this.settleTimer);
            this.tail.close();
            if (drivers.get(localId) === this) drivers.delete(localId);
          },
        };

        handle.setWorking(proc?.isBusy() ?? false, { broadcast: false });
        handle.setOrchestrator(
          {
            active: proc?.lastOrchestratorActive ?? false,
            widget: proc?.lastOrchestratorWidget ?? null,
            status: proc?.lastOrchestratorStatus ?? null,
          },
          { broadcast: false }
        );
        driver.tail = createFileTail(sessionFile, {
          onLines: (lines, initial) => driver.onLines(lines, initial),
          onReset: () => {
            // Compaction (or any rewrite) invalidates every parsed entry and
            // the overlay alike; the follow-up initial delivery rebuilds and
            // re-broadcasts a snapshot.
            driver.entries = [];
            driver.reset = true;
            handle.resetTranscript();
          },
        });
        drivers.set(localId, driver);
        driver.tail.start();
        return driver;
      };
    },

    shutdown() {
      pool.shutdown();
    },
  };
}
