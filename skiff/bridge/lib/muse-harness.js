// bridge/lib/muse-harness.js
// The muse harness: skiff sessions backed by Meta's Muse Code. The store
// (lib/muse-store.js) reads muse's event-sourced session files; the runner
// (lib/muse-run.js) drives `muse exec --json --yolo` children — one per
// in-flight prompt — whose stdout feeds liveness and the streamed-output
// overlay. Committed messages only ever come from the session file.
//
// Capabilities: no rename (muse names sessions itself — session.name.changed
// records with source "automatic" — and exposes no rename command), and no
// orchestrator (a pi extension). Reasoning never renders: muse persists it
// encrypted.
//
// Session dir override (tests): when config.sessionDir is set explicitly it
// must be <root>/muse/sessions, and spawned muse children get
// XDG_DATA_HOME=<root> — the mechanism muse itself resolves its data dir by
// — so scanning and writing stay in one place. The default deployment leaves
// it unset and shares muse's native dir with the muse CLI.

import path from "node:path";
import {
  defaultMuseSessionDir,
  resolveMuseSessionFile,
  readMuseSessionFile,
  buildMuseSessionObject,
  mapMuseMessages,
  listMuseSessions,
  parseLine,
} from "./muse-store.js";
import { MuseRunner } from "./muse-run.js";
import { createFileTail } from "./file-tail.js";
import { HttpError } from "./errors.js";
import { resolveBinary } from "./resolve-binary.js";

// Same one-deferred-retry window as the pi driver's settle scan: the child's
// exit and the file's last append can race the watcher by a few
// milliseconds.
const SETTLE_SCAN_DELAY_MS = 300;

export function createMuseHarness(config, { defaultCwd }) {
  const sessionDir = config.sessionDir ?? defaultMuseSessionDir();
  let spawnEnv = null;
  if (config.sessionDir) {
    // <root>/muse/sessions -> XDG_DATA_HOME=<root>; anything else cannot be
    // made consistent for spawned children, so it is a configuration error.
    const museDir = path.dirname(sessionDir);
    const root = path.dirname(museDir);
    if (path.basename(sessionDir) !== "sessions" || path.basename(museDir) !== "muse") {
      throw new Error(
        `skiff-bridge: an explicit muse session dir must end in /muse/sessions (got ${sessionDir}); ` +
          "spawned muse children resolve it via XDG_DATA_HOME and would otherwise write elsewhere"
      );
    }
    spawnEnv = { XDG_DATA_HOME: root };
  }
  const runner = new MuseRunner({
    binary: resolveBinary("muse", config.binary ?? process.env.MUSE_BINARY),
    defaultCwd,
    spawnEnv,
  });

  function newbornSessionObject(id, newborn) {
    return {
      id,
      title: null, // muse names the session itself once the first run boots
      directory: newborn.cwd,
      time: { created: newborn.createdAt, updated: newborn.createdAt },
      model: null,
    };
  }

  // Resolve a session to its file or its newborn record. The file wins; a
  // newborn whose file has appeared is pruned on sight — its purpose (keeping
  // a created-but-never-persisted session visible) ends with the file.
  async function resolveTarget(localId) {
    const file = await resolveMuseSessionFile(sessionDir, localId);
    if (file) {
      runner.newborns.delete(localId);
      const parsed = await readMuseSessionFile(file);
      if (parsed) return { kind: "file", file, parsed };
      return null; // a directory with an unreadable/empty session log
    }
    const newborn = runner.newborn(localId);
    if (newborn) return { kind: "newborn", newborn };
    return null;
  }

  return {
    name: "muse",
    capabilities: { rename: false, orchestrator: false },
    runner, // exposed for tests (run introspection) and shutdown wiring

    async listSessions() {
      const sessions = await listMuseSessions(sessionDir);
      const ids = new Set(sessions.map((s) => s.id));
      for (const [id, newborn] of runner.newborns) {
        if (ids.has(id)) {
          runner.newborns.delete(id); // the file exists now; the record is spent
          continue;
        }
        sessions.push(newbornSessionObject(id, newborn));
      }
      return sessions;
    },

    async getSession(localId) {
      const target = await resolveTarget(localId);
      if (!target) return null;
      if (target.kind === "newborn") return newbornSessionObject(localId, target.newborn);
      return buildMuseSessionObject(localId, target.parsed);
    },

    async getMessages(localId) {
      const target = await resolveTarget(localId);
      if (!target) return null;
      const messages = target.kind === "file" ? mapMuseMessages(target.parsed.records) : [];
      // Streaming overlay: while a run for this session is mid-stream, its
      // accumulated output is appended after the committed messages — same
      // composition as the pi harness.
      const pending = runner.run(localId)?.pendingEntry();
      if (pending) messages.push(pending);
      return messages;
    },

    // No process is spawned at create time: muse accepts any uuid via
    // --session-id, so the session is just a minted id plus the cwd its
    // first run will use. The title is ignored — muse names sessions itself
    // and has no rename surface (capabilities.rename is false, so skiff
    // never offers one).
    async createSession() {
      return runner.createSession(defaultCwd);
    },

    async promptAsync(localId, text) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session muse:${localId} not found`);
      // Runs execute in the session's own workspace: muse trusts and roots
      // itself at the spawn cwd, so a session started in ~/code/blog keeps
      // running there whichever machine corner the phone drives it from.
      const cwd =
        target.kind === "newborn"
          ? target.newborn.cwd
          : buildMuseSessionObject(localId, target.parsed).directory ?? defaultCwd;
      await runner.prompt(localId, text, cwd);
    },

    async abort(localId) {
      const target = await resolveTarget(localId);
      if (!target) throw new HttpError(404, `session muse:${localId} not found`);
      runner.abort(localId); // idle sessions are a no-op, like pi's abort
    },

    busyLocalIds() {
      return runner.busyIds();
    },

    // The stream driver. Sources mirror the poll composition:
    //
    //  1. The session file (via lib/file-tail.js), mapped by the muse store.
    //     An unborn file's PATH is unknowable up front — muse picks the
    //     dated directory (sessions/YYYY/MM/DD/<uuid>/) when the first run
    //     boots — so resolution is event-driven: each runner event retries
    //     the store lookup until the file appears, then the tail starts.
    //  2. The runner's live events: spawned (working on), delta (overlay
    //     growth), exit (working off; unresolved overlay settles via one
    //     scan plus one deferred retry, then drops — an interrupted muse run
    //     leaves no terminal record, so exit is the only end-of-run signal).
    //
    // Resolution rule: a committed assistant entry WITH TEXT at the
    // overlay's index resolves it (the streamed output is that message's
    // text); a tool-call batch is also assistant-role but never the streamed
    // text, so it kicks the overlay and appends — the buffer survives in the
    // run and the overlay re-opens on the next delta. On resolution the run
    // trims the committed text off its buffer, so an intermediate message
    // never duplicates into the next overlay.
    async resolveStreamDriver(localId) {
      const file = await resolveMuseSessionFile(sessionDir, localId);
      if (!file && !runner.newborn(localId) && !runner.run(localId)) return null;

      return (handle) => {
        const driver = {
          records: [],
          // The FIRST delivery always rebuilds into a snapshot, however it
          // arrives (see the pi driver: the unborn-file path delivers the
          // initial read through the watcher, flagged initial: false).
          reset: true,
          tail: null,
          settleTimer: null,
          closed: false,
          unsubscribe: null,

          overlayEntry() {
            return runner.run(localId)?.pendingEntry() ?? null;
          },

          resolvesOverlay(entry) {
            return entry.info.role === "assistant" && entry.parts.some((part) => part.type === "text");
          },

          onOverlayResolved(entry) {
            const text = entry.parts
              .filter((part) => part.type === "text")
              .map((part) => part.text)
              .join("\n");
            runner.run(localId)?.consumeResolvedText(text);
          },

          startTail(sessionFile) {
            if (this.tail || this.closed) return;
            this.tail = createFileTail(sessionFile, {
              onLines: (lines, initial) => this.onLines(lines, initial),
              onReset: () => {
                // Muse never rewrites a session log today; if it ever does
                // (a future compaction), the reset path rebuilds and
                // re-snapshots exactly like the pi driver.
                this.records = [];
                this.reset = true;
                handle.resetTranscript();
              },
            });
            this.tail.start();
          },

          async resolveTailIfNeeded() {
            if (this.tail || this.closed) return;
            const sessionFile = await resolveMuseSessionFile(sessionDir, localId);
            if (sessionFile && !this.closed && !this.tail) this.startTail(sessionFile);
          },

          onLines(lines, initial) {
            for (const line of lines) {
              const record = parseLine(line);
              if (record && typeof record.payload_type === "string") this.records.push(record);
            }
            if (initial || this.reset) {
              this.reset = false;
              handle.deliverTranscript(mapMuseMessages(this.records), { snapshot: true });
              return;
            }
            handle.deliverTranscript(mapMuseMessages(this.records));
          },

          onRunnerEvent(event) {
            switch (event.type) {
              case "spawned":
                handle.setWorking(true);
                this.resolveTailIfNeeded();
                break;
              case "delta":
                this.resolveTailIfNeeded();
                if (!handle.hasOverlay()) handle.overlayOpen();
                handle.overlayChanged();
                break;
              case "exit":
                handle.setWorking(false);
                this.resolveTailIfNeeded().then(() => {
                  if (this.closed || !handle.hasOverlay()) return;
                  // The last committed entry can race the watcher; scan now,
                  // then once more after a beat, before concluding the
                  // overlay text never persisted (an interrupted run).
                  this.tail?.scan();
                  if (!handle.hasOverlay() || this.settleTimer) return;
                  this.settleTimer = setTimeout(() => {
                    this.settleTimer = null;
                    if (this.closed || !handle.hasOverlay()) return;
                    this.tail?.scan();
                    if (handle.hasOverlay()) handle.overlayDrop();
                  }, SETTLE_SCAN_DELAY_MS);
                  this.settleTimer.unref?.();
                });
                break;
              default:
                break; // terminal: informational; exit follows and owns convergence
            }
          },

          close() {
            this.closed = true;
            if (this.settleTimer) clearTimeout(this.settleTimer);
            this.unsubscribe?.();
            this.tail?.close();
          },
        };

        handle.setWorking(runner.isBusy(localId), { broadcast: false });
        driver.unsubscribe = runner.subscribe(localId, (event) => driver.onRunnerEvent(event));
        if (file) driver.startTail(file);
        return driver;
      };
    },

    shutdown() {
      runner.shutdown();
    },
  };
}
