// bridge/lib/muse-run.js
// The live side of the muse harness: one `muse exec --json` child per
// in-flight prompt, plus the newborn-session bookkeeping.
//
// Muse has no long-lived RPC mode (pi's `--mode rpc` has no muse
// equivalent); its headless surface is one process per run that resumes the
// session by id, streams machine-readable JSONL on stdout, and persists the
// committed transcript into the session file. Facts this module leans on
// (verified live against Muse Code 0.2.1):
//
// - `muse exec --json --session-id <uuid>` RESUMES an existing session (the
//   second run's records continue the same session.jsonl, `resume: true`).
// - `run.output.delta` events carry INCREMENTAL text chunks of the run's
//   streamed output; accumulation assembles the in-flight assistant text.
// - Committed transcript events (assistant_message_committed, tool batches)
//   appear only in the session file, never on exec stdout — so stdout feeds
//   liveness and the overlay, and the file (via the harness's tail) is the
//   only source of committed messages.
// - An interrupted run (SIGINT) exits WITHOUT a terminal record, on stdout
//   or in the file; the child's exit is the only end-of-run signal, so the
//   exit handler owns convergence (working off, overlay dropped). A later
//   run on the same session recovers cleanly (muse handles the stale
//   session lock).
//
// The prompt travels via --prompt-file, never argv: argv is visible to every
// process on the host for the run's whole duration, and the bridge's
// security posture is that prompt text never reaches logs or other
// processes. The file lives in a mode-0700 temp dir removed when the child
// exits.
//
// Safety posture: bridge-driven runs pass --yolo (approval and sandbox off,
// workspace trusted) — the same full-autonomy model as driving pi from the
// phone; there is no human at an approval prompt on the other end of this
// bridge.

import { spawn } from "node:child_process";
import { StringDecoder } from "node:string_decoder";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createJsonlReader } from "./jsonl.js";
import { HttpError } from "./errors.js";

const PENDING_MESSAGE_ID = "<pending>";

export class MuseError extends Error {}

class MuseRun {
  constructor({ binary, sessionId, cwd, env, onEvent, onExit }) {
    this.binary = binary;
    this.sessionId = sessionId;
    this.cwd = cwd;
    this.env = env;
    this.onEvent = onEvent;
    this.onExit = onExit;
    this.child = null;
    this.exited = false;
    this.startedAt = Date.now();
    this.outputText = ""; // accumulated run.output.delta chunks
    this.sawRecord = false;
    this.stderrTail = "";
    this.tmpDir = null;
    this.readyResolve = null;
    this.readyReject = null;
  }

  // Spawn the child and resolve once muse acknowledges the run (its first
  // stdout record), or reject when it exits before ever speaking — the
  // stderr tail then carries muse's own refusal (bad session id, provider
  // mismatch, …). No timeout: muse either prints or exits.
  start(promptText) {
    this.tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "skiff-muse-"));
    fs.chmodSync(this.tmpDir, 0o700);
    const promptFile = path.join(this.tmpDir, "prompt.txt");
    fs.writeFileSync(promptFile, promptText, { mode: 0o600 });

    const args = ["exec", "--json", "--yolo", "--session-id", this.sessionId, "--prompt-file", promptFile];
    const child = spawn(this.binary, args, { cwd: this.cwd, env: this.env, stdio: ["ignore", "pipe", "pipe"] });
    this.child = child;

    createJsonlReader(child.stdout, (line) => this._onLine(line));
    const decoder = new StringDecoder("utf8");
    child.stderr.on("data", (chunk) => {
      this.stderrTail = (this.stderrTail + decoder.write(chunk)).slice(-4096);
    });
    child.on("error", (err) => this._handleExit(`spawn failed: ${err.code ?? err.message}`));
    child.on("exit", (code, signal) => {
      const reason = code === 0 ? "clean exit" : `exit code ${code}${signal ? ` (${signal})` : ""}`;
      this._handleExit(reason);
    });

    return new Promise((resolve, reject) => {
      this.readyResolve = resolve;
      this.readyReject = reject;
    });
  }

  _onLine(line) {
    let record;
    try {
      record = JSON.parse(line);
    } catch {
      return; // an unparseable line from muse is not worth surfacing
    }
    if (!this.sawRecord) {
      this.sawRecord = true;
      this.readyResolve?.();
      this.readyResolve = null;
      this.readyReject = null;
      this.onEvent({ type: "spawned" });
    }
    if (record.payload_type === "run.output.delta" && typeof record.payload?.text === "string") {
      this.outputText += record.payload.text;
      this.onEvent({ type: "delta" });
    } else if (typeof record.payload_type === "string" && record.payload_type.startsWith("run.terminal.")) {
      this.onEvent({ type: "terminal", terminal: record.payload?.terminal ?? null });
    }
  }

  _handleExit(reason) {
    if (this.exited) return;
    this.exited = true;
    if (this.tmpDir) fs.rmSync(this.tmpDir, { recursive: true, force: true });
    if (this.readyReject) {
      const detail = this.stderrTail.trim().split("\n").at(-1) ?? "";
      this.readyReject(new MuseError(`muse ${reason}${detail ? `: ${detail}` : ""}`));
      this.readyReject = null;
      this.readyResolve = null;
    }
    this.onExit(this, reason);
  }

  abort() {
    if (this.child && !this.exited) this.child.kill("SIGINT");
  }

  kill() {
    if (this.child && !this.exited) this.child.kill("SIGTERM");
  }

  // The overlay message: the accumulated streamed output as an in-flight
  // assistant entry, with no time.completed so skiff's working indicator
  // fires. Null while nothing has streamed — an empty bubble is noise.
  pendingEntry() {
    if (this.outputText === "") return null;
    return {
      info: { id: PENDING_MESSAGE_ID, role: "assistant", agent: null, time: { created: this.startedAt } },
      parts: [{ type: "text", text: this.outputText, id: `${PENDING_MESSAGE_ID}-p0` }],
    };
  }

  // Called when a committed assistant message resolved the overlay: the
  // committed text is the front of the accumulated stream, so it is trimmed
  // off and any remainder (a run that keeps streaming after an intermediate
  // message) re-opens as a fresh overlay on the next delta.
  consumeResolvedText(text) {
    if (text && this.outputText.startsWith(text)) {
      this.outputText = this.outputText.slice(text.length).replace(/^\s+/, "");
    } else {
      this.outputText = "";
    }
  }
}

export class MuseRunner {
  constructor({ binary, defaultCwd, spawnEnv = null }) {
    this.binary = binary;
    this.defaultCwd = defaultCwd;
    this.spawnEnv = spawnEnv; // extra env for spawned muse (XDG override in tests)
    this.runs = new Map(); // session id -> MuseRun
    this.newborns = new Map(); // session id -> { cwd, createdAt }
    this.listeners = new Map(); // session id -> Set<fn>
  }

  // A newborn session exists only in this map until its first run makes muse
  // write the session directory. The id is minted here — muse accepts any
  // uuid via --session-id — so no process is spawned at create time at all.
  createSession(cwd = this.defaultCwd) {
    const id = randomUUID();
    this.newborns.set(id, { cwd, createdAt: Date.now() });
    return id;
  }

  newborn(id) {
    return this.newborns.get(id) ?? null;
  }

  isBusy(id) {
    return this.runs.has(id);
  }

  busyIds() {
    return [...this.runs.keys()];
  }

  run(id) {
    return this.runs.get(id) ?? null;
  }

  // One run per session at a time: muse locks the session dir, and a second
  // exec would just die on the lock — reject it honestly instead.
  async prompt(sessionId, text, cwd) {
    if (this.runs.has(sessionId)) {
      throw new HttpError(409, "a run is already active for this session");
    }
    const run = new MuseRun({
      binary: this.binary,
      sessionId,
      cwd,
      env: this.spawnEnv ? { ...process.env, ...this.spawnEnv } : process.env,
      onEvent: (event) => this._emit(sessionId, event),
      onExit: (r, reason) => {
        if (this.runs.get(sessionId) === r) this.runs.delete(sessionId);
        this._emit(sessionId, { type: "exit", reason });
      },
    });
    this.runs.set(sessionId, run);
    try {
      await run.start(text);
    } catch (err) {
      throw new HttpError(502, `prompt failed: ${err.message}`);
    }
    // The newborn record is NOT dropped here: muse may not have written the
    // session file yet when it acknowledges the run, and a session must
    // never be invisible in that window. The harness prunes the record the
    // moment the file exists (see muse-harness.js); a run that dies before
    // ever persisting leaves the newborn in place, so the created session
    // stays visible and promptable.
  }

  // Aborting an idle session is a no-op, like pi's abort command on an idle
  // process: the intent — nothing running afterwards — already holds.
  abort(sessionId) {
    this.runs.get(sessionId)?.abort();
  }

  subscribe(sessionId, fn) {
    let set = this.listeners.get(sessionId);
    if (!set) {
      set = new Set();
      this.listeners.set(sessionId, set);
    }
    set.add(fn);
    return () => {
      set.delete(fn);
      if (set.size === 0) this.listeners.delete(sessionId);
    };
  }

  _emit(sessionId, event) {
    for (const fn of this.listeners.get(sessionId) ?? []) fn(event);
  }

  shutdown() {
    for (const run of this.runs.values()) run.kill();
    this.runs.clear();
    this.newborns.clear();
  }
}
