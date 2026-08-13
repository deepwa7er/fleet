// bridge/lib/stream-registry.js
// The push side of the bridge: per-session live state fanned out to SSE
// subscribers. Where the poll endpoints compose file state with process
// state per request, the registry holds the composition continuously — the
// mapped transcript, the in-flight assistant overlay, the orchestrator
// readout — and emits events only when something actually changed.
//
// Event protocol (SSE frames, JSON payloads; consumed by skiff's
// sessions#stream action, which translates them into turbo-streams):
//
//   snapshot      { messages, pending: {index, entry}|null, working,
//                   orchestrator: {active, widget, status} }
//                 sent on every (re)build of the transcript state: the
//                 initial delivery, a compaction reset, and nothing else —
//                 incremental changes always arrive as the ops below.
//   append        { index, entry }   a new message at the end of the file
//   replace       { index, entry }   a message's content changed (in-flight
//                                    overlay growth, or the overlay resolving
//                                    into its persisted entry)
//   remove        { index }          a message is gone (aborted run)
//   working       { working }        agent_start / agent_settled
//   orchestrator  { orchestrator }   mode toggle or widget publication
//
// Two sources feed the state, mirroring the poll endpoints' composition:
//
//  1. The file (via lib/file-tail.js). pi persists entries append-only and
//     rewrites on compaction; every delivery is mapped with the session-store
//     (same leaf-branch and toolResult-folding rules the HTTP layer uses) and
//     diffed index-wise against the previous transcript. The diff is exact —
//     messages never reorder, ids are stable, and a toolResult fold mutates
//     only the message that owns the tool part — so a plain equality compare
//     emits exactly the ops the view needs, with no fingerprints.
//
//  2. The live pi process (via the pool's processEvent hook, wired in
//     server.js). Only assistant message events matter: the overlay's index
//     is pinned at message_start, its growth is broadcast coalesced
//     (FLUSH_INTERVAL_MS — a per-token replace would re-render the view at
//     token frequency), and message_end marks it "resolving".
//
// Resolution rule (the one piece of reconciliation that must survive — it is
// the bridge's only structural guess, and it is deterministic): pi persists
// the assistant entry exactly at message_end, and nothing else can append
// while the overlay is live, so the next file entry landing at the overlay's
// index is the same message. An assistant entry there replaces the overlay
// in place; anything else (a late-delivered pre-run entry — the watcher can
// lag the pipe) kicks the overlay and appends normally. An agent_end without
// message_end means the message never persisted: the overlay is removed.
// message_end and the watcher can race, so agent_end/agent_settled force a
// synchronous file scan before concluding.
//
// Lifecycle: a session's state exists while it has subscribers; the file
// tail opens on the first and closes on the last. While subscribed, the
// session's process is pinned in the pool (isPinned) so an idle session
// cannot be LRU-evicted out from under its own stream.

import path from "node:path";
import { resolveSessionFile, mapMessages, parseLine, orchestratorActiveFromEntries } from "./session-store.js";
import { createFileTail } from "./file-tail.js";

const FLUSH_INTERVAL_MS = 100;
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

export function createStreamRegistry({ pool, sessionDir }) {
  const sessions = new Map(); // session id -> state

  function createState(id, file, proc) {
    return {
      id,
      file,
      proc,
      tail: null,
      entries: [], // raw parsed entries, append order (the file's truth)
      messages: [], // mapped transcript (mapMessages of entries)
      pending: null, // { index, state: "live" | "resolving" }
      reset: true, // next line delivery rebuilds instead of diffing
      subscribers: new Set(),
      working: proc?.isBusy() ?? false,
      orchestrator: {
        active: proc?.lastOrchestratorActive ?? false,
        widget: proc?.lastOrchestratorWidget ?? null,
        status: proc?.lastOrchestratorStatus ?? null,
      },
      flushTimer: null,
    };
  }

  async function subscribe(id, res) {
    // A session's state exists while it has subscribers; a later subscriber
    // joins the existing state (the tail keeps watching, the newcomer gets
    // a snapshot of the current state) rather than creating a second one
    // that would split the event stream across two states.
    const existing = sessions.get(id);
    if (existing) return joinState(existing, id, res);

    const file = await resolveSessionFile(sessionDir, id);
    const proc = file ? pool.getByFile(file) : pool.getById(id);
    if (!file && !proc) return false;

    // A concurrent subscribe may have created the state while this one was
    // resolving the file; join it rather than creating a duplicate.
    const raced = sessions.get(id);
    if (raced) return joinState(raced, id, res);

    const state = createState(id, file ?? proc.sessionFile, proc);
    sessions.set(id, state);
    state.subscribers.add(res);
    res.on("close", () => unsubscribe(id, res));
    writeSseHeaders(res);

    state.tail = createFileTail(state.file, {
      onLines: (lines, initial) => ingestLines(state, lines, initial),
      onReset: () => {
        // Compaction (or any rewrite) invalidates every parsed entry and the
        // overlay alike; the follow-up initial delivery rebuilds and
        // re-broadcasts a snapshot.
        state.entries = [];
        state.messages = [];
        state.pending = null;
        state.reset = true;
        cancelFlush(state);
      },
    });
    state.tail.start();
    return true;
  }

  function joinState(state, id, res) {
    writeSseHeaders(res);
    state.subscribers.add(res);
    res.on("close", () => unsubscribe(id, res));
    sendSnapshot(state, res);
    return true;
  }

  function writeSseHeaders(res) {
    res.writeHead(200, {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    });
    res.flushHeaders();
  }

  function unsubscribe(id, res) {
    const state = sessions.get(id);
    if (!state) return;
    state.subscribers.delete(res);
    if (state.subscribers.size === 0) {
      cancelFlush(state);
      state.tail?.close();
      sessions.delete(id);
    }
  }

  function close() {
    for (const state of sessions.values()) {
      cancelFlush(state);
      state.tail?.close();
      for (const res of state.subscribers) {
        if (!res.writableEnded) res.end();
      }
    }
    sessions.clear();
  }

  function hasSubscribers(sessionFile) {
    if (!sessionFile) return false;
    const state = sessions.get(basenameId(sessionFile));
    return Boolean(state && state.subscribers.size > 0);
  }

  // --- file side ------------------------------------------------------------

  function ingestLines(state, lines, initial) {
    for (const line of lines) {
      const entry = parseLine(line);
      if (!entry) continue;
      state.entries.push(entry);
      if (entry.type === "custom" && entry.customType === "orchestrator-mode") {
        // File-only sessions (no live process) still see mode toggles; the
        // live path also delivers them via entry_appended, so both sources
        // converge on the same value.
        state.orchestrator.active = entry.data?.active === true;
      }
    }
    if (initial || state.reset) {
      state.reset = false;
      state.messages = mapMessages(state.entries);
      state.orchestrator.active = orchestratorActiveFromEntries(state.entries);
      sendSnapshot(state);
      return;
    }
    applyDiff(state);
  }

  // Index-wise diff of the re-mapped transcript against the previous one.
  // Entries never reorder and ids are stable, so every difference is one of:
  // a new trailing message (append), a toolResult fold mutating one message
  // in place (replace), or — only on a branch change — a message vanishing or
  // being replaced at its index. The overlay occupies the last bubble, and an
  // append landing at its index is its own persistence (see the module
  // comment for the ordering that makes this deterministic). Anything else
  // emitted while the overlay lives — a fold delivered late, a branch
  // change — means the view's bubble is no longer where the registry thinks
  // it is, so the whole transcript is re-snapshotted instead of patched.
  function applyDiff(state) {
    const fresh = mapMessages(state.entries);
    const previous = state.messages;
    const length = Math.max(previous.length, fresh.length);
    let diverged = false;
    for (let i = 0; i < length; i++) {
      const before = previous[i];
      const after = fresh[i];
      if (!after) {
        broadcast(state, "remove", { index: i });
        diverged = true;
      } else if (!before) {
        appendOrResolve(state, i, after);
      } else if (JSON.stringify(before) !== JSON.stringify(after)) {
        broadcast(state, "replace", { index: i, entry: after });
        diverged = true;
      }
    }
    state.messages = fresh;
    if (diverged && state.pending) {
      // The patch path cannot place the overlay correctly anymore; a
      // snapshot re-renders it at the transcript's new length.
      sendSnapshot(state);
    }
  }

  function appendOrResolve(state, index, entry) {
    if (state.pending && index === state.pending.index) {
      if (entry.info.role === "assistant") {
        if (state.pending.broadcast) {
          // The overlay rendered: replace the streaming bubble in place with
          // the authoritative entry (real id, time.completed).
          broadcast(state, "replace", { index, entry });
        } else {
          // The overlay never rendered — the run finished before the first
          // coalesced flush (a short reply). The view has no bubble at this
          // index, so a replace would target nothing and lose the message;
          // append it like any new entry.
          broadcast(state, "append", { index, entry });
        }
      } else {
        // A pre-run entry the watcher delivered late (it cannot be anything
        // else: pi appends nothing while the overlay streams). The overlay
        // bubble was never this entry — kick it and append normally.
        broadcast(state, "remove", { index });
        broadcast(state, "append", { index, entry });
      }
      state.pending = null;
      return;
    }
    broadcast(state, "append", { index, entry });
  }

  // --- process side ---------------------------------------------------------

  function onProcessEvent(proc, event) {
    if (!proc.sessionFile) return; // a bare create-flow process has no session identity
    const state = sessions.get(basenameId(proc.sessionFile));
    if (!state) return;
    state.proc = proc;

    switch (event.type) {
      case "message_start":
        if (event.message?.role === "assistant") {
          // The overlay is always the last bubble. Its index is not stable
          // until the first broadcast: the file watcher can lag the pipe (a
          // pre-run entry still unwatched when the message begins), so the
          // position is re-pinned at every broadcast to the transcript's
          // current length — the spot where the snapshot or flush renders
          // it. broadcast flips when the overlay has been rendered anywhere,
          // so a resolution replaces the bubble when there is one to
          // replace, and appends otherwise.
          state.pending = { index: state.messages.length, state: "live", broadcast: false };
        }
        break;
      case "message_update":
        if (state.pending?.state === "live") scheduleFlush(state);
        break;
      case "message_end":
        if (state.pending) {
          // The entry lands on disk with message_end (verified against real
          // pi); the file side resolves the overlay when it scans it.
          state.pending.state = "resolving";
          cancelFlush(state);
        }
        break;
      case "agent_start":
        state.working = true;
        broadcast(state, "working", { working: true });
        break;
      case "agent_end": {
        if (!state.pending) break;
        if (state.pending.state === "live") {
          // The run ended without message_end: the message never persisted.
          cancelFlush(state);
          broadcast(state, "remove", { index: state.pending.index });
          state.pending = null;
        } else {
          // Resolving: the persisted entry raced the watcher; scan the file
          // to settle it now rather than leaving the bubble indeterminate.
          state.tail?.scan();
        }
        break;
      }
      case "agent_settled":
        state.working = false;
        broadcast(state, "working", { working: false });
        if (state.pending) {
          state.tail?.scan(); // the entry should be on disk; the watcher may lag the pipe
          if (state.pending) {
            // The write can still race the scan (pi persists concurrently
            // with message_end); one deferred scan before concluding.
            setTimeout(() => {
              const still = sessions.get(state.id);
              if (!still || still !== state || !state.pending) return;
              state.tail?.scan();
              if (state.pending) {
                cancelFlush(state);
                broadcast(state, "remove", { index: state.pending.index });
                state.pending = null;
              }
            }, SETTLE_SCAN_DELAY_MS).unref?.();
          }
        }
        break;
      case "entry_appended":
        if (event.entry?.customType === "orchestrator-mode") {
          state.orchestrator.active = event.entry.data?.active === true;
          broadcastOrchestrator(state);
        }
        break;
      case "extension_ui_request":
        // setWidget/setStatus for the orchestrator key are the live plan and
        // worker readout; the process captured them before this hook ran.
        if (event.method === "setWidget" || event.method === "setStatus") {
          broadcastOrchestrator(state);
        }
        break;
      case "process_exit":
        // The process died mid-run (crash or shutdown). The overlay can no
        // longer resolve through deltas; if the message persists, the file
        // side appends it normally, so dropping the overlay is safe.
        if (state.pending) {
          cancelFlush(state);
          broadcast(state, "remove", { index: state.pending.index });
          state.pending = null;
        }
        break;
      default:
        break; // responses, tool_execution_*, queue_update, compaction_*, …
    }
  }

  // Coalesced overlay broadcast: deltas arrive at token frequency, but the
  // view re-renders at most FLUSH_INTERVAL_MS apart. Each flush sends the
  // fully assembled overlay (PiProcess.getPendingMessage), not a delta, so
  // the consumer's renderer never assembles anything. The index is re-pinned
  // to the transcript's current length — the position where the view holds
  // the bubble — so a watcher that lagged the pipe cannot drift the target.
  function scheduleFlush(state) {
    if (state.flushTimer) return;
    state.flushTimer = setTimeout(() => {
      state.flushTimer = null;
      const entry = state.proc?.getPendingMessage();
      if (state.pending?.state === "live" && entry) {
        state.pending.index = state.messages.length;
        state.pending.broadcast = true; // the view now holds the overlay bubble
        broadcast(state, "replace", { index: state.pending.index, entry });
      }
    }, FLUSH_INTERVAL_MS);
  }

  function cancelFlush(state) {
    if (state.flushTimer) {
      clearTimeout(state.flushTimer);
      state.flushTimer = null;
    }
  }

  // --- broadcast ------------------------------------------------------------

  function sendSnapshot(state, onlyRes = null) {
    const pendingEntry = state.pending ? state.proc?.getPendingMessage() : null;
    if (state.pending && !pendingEntry) state.pending = null; // the overlay is gone
    if (state.pending) {
      // The snapshot renders the overlay after the file messages, so its
      // position is the transcript's current length — re-pin, exactly like
      // the flushes do.
      state.pending.index = state.messages.length;
      state.pending.broadcast = true; // the snapshot renders it
    }
    broadcast(state, "snapshot", {
      messages: state.messages,
      pending: state.pending ? { index: state.pending.index, entry: pendingEntry } : null,
      working: state.working,
      orchestrator: state.orchestrator,
    }, onlyRes);
  }

  function broadcastOrchestrator(state) {
    const proc = state.proc;
    if (!proc) return;
    state.orchestrator = {
      active: state.orchestrator.active,
      widget: proc.lastOrchestratorWidget,
      status: proc.lastOrchestratorStatus,
    };
    broadcast(state, "orchestrator", { orchestrator: state.orchestrator });
  }

  function broadcast(state, name, payload, onlyRes = null) {
    for (const res of state.subscribers) {
      if (onlyRes && res !== onlyRes) continue;
      sendEvent(res, name, payload);
    }
  }

  function sendEvent(res, name, payload) {
    if (res.writableEnded) return;
    try {
      res.write(`event: ${name}\ndata: ${JSON.stringify(payload)}\n\n`);
    } catch {
      // the response's close handler owns teardown
    }
  }

  return { subscribe, close, hasSubscribers, onProcessEvent };
}
