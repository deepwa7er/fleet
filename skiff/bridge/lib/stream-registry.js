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
//   working       { working }        the harness's run started / settled
//   orchestrator  { orchestrator }   mode toggle or widget publication (pi)
//
// The registry is harness-agnostic: it owns the subscribers, the index-wise
// transcript diff, the overlay's position bookkeeping, and the coalesced
// flushing. Everything harness-specific — where the transcript comes from,
// what the overlay contains, when a run is working — lives in a per-session
// DRIVER the harness supplies at subscribe time.
//
// Driver contract. `resolveDriver()` (async, side-effect free) answers null
// for an unknown session or a `create` function; `create(handle)` (sync)
// attaches the driver's sources — file tails, process hooks — and returns:
//
//   overlayEntry()          -> {info, parts} | null  the in-flight assistant
//                              message, assembled by the harness
//   resolvesOverlay(entry)  -> bool  does this newly-appended transcript
//                              entry persist the live overlay? (pi: any
//                              assistant entry — nothing else can append
//                              while an overlay streams; muse: an assistant
//                              entry with text — tool-call batches append
//                              mid-run without ending the streamed text)
//   onOverlayResolved(entry)   optional; called after a resolution replaced
//                              the overlay bubble, so the harness can reset
//                              its delta accumulation
//   close()                    detach everything
//
// The handle passed to `create` is the driver's write surface onto the
// session state:
//
//   deliverTranscript(messages, {snapshot}) diff (or snapshot) the re-mapped
//                                           transcript against the last one
//   resetTranscript()                       compaction: forget everything;
//                                           the next delivery must snapshot
//   overlayOpen()                           pin the overlay at the end
//   overlayChanged()                        schedule a coalesced flush
//   overlayResolving()                      stop flushing; the file side owns
//                                           the resolution now
//   overlayDrop()                           remove the bubble (aborted run)
//   hasOverlay()                            is an overlay live or resolving?
//   setWorking(working, {broadcast})
//   setOrchestrator(patch, {broadcast})     merge into the readout
//
// Index semantics: a message's index is its position in the mapped
// transcript array. The overlay is always the last bubble; its index is
// re-pinned to the transcript's current length at every broadcast (snapshot
// or flush), so a file watcher that lags the live events cannot drift the
// target. `broadcast` on the pending record flips once the overlay has been
// rendered anywhere — a resolution replaces the bubble when there is one to
// replace, and appends otherwise.
//
// Lifecycle: a session's state exists while it has subscribers; the driver
// opens on the first and closes on the last.

const FLUSH_INTERVAL_MS = 100;

export function createStreamRegistry() {
  const sessions = new Map(); // session key -> state

  function createState(key) {
    return {
      key,
      driver: null,
      messages: [], // mapped transcript, the driver's last delivery
      pending: null, // { index, broadcast }
      subscribers: new Set(),
      working: false,
      orchestrator: { active: false, widget: null, status: null },
      flushTimer: null,
    };
  }

  // Subscribe `res` to the session behind `key`. `resolveDriver` is the
  // harness's async resolution (null -> 404); on success its `create` runs
  // exactly once per state — a later subscriber joins the existing state
  // (the driver keeps watching, the newcomer gets a snapshot) rather than
  // creating a second one that would split the event stream.
  async function subscribe(key, res, resolveDriver) {
    const existing = sessions.get(key);
    if (existing) return joinState(existing, key, res);

    const create = await resolveDriver();
    if (!create) return false;

    // A concurrent subscribe may have created the state while this one was
    // resolving; join it rather than creating a duplicate.
    const raced = sessions.get(key);
    if (raced) return joinState(raced, key, res);

    const state = createState(key);
    sessions.set(key, state);
    state.subscribers.add(res);
    res.on("close", () => unsubscribe(key, res));
    writeSseHeaders(res);
    state.driver = create(makeHandle(state));
    return true;
  }

  function joinState(state, key, res) {
    writeSseHeaders(res);
    state.subscribers.add(res);
    res.on("close", () => unsubscribe(key, res));
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

  function unsubscribe(key, res) {
    const state = sessions.get(key);
    if (!state) return;
    state.subscribers.delete(res);
    if (state.subscribers.size === 0) {
      cancelFlush(state);
      state.driver?.close();
      sessions.delete(key);
    }
  }

  function close() {
    for (const state of sessions.values()) {
      cancelFlush(state);
      state.driver?.close();
      for (const res of state.subscribers) {
        if (!res.writableEnded) res.end();
      }
    }
    sessions.clear();
  }

  function hasSubscribers(key) {
    const state = sessions.get(key);
    return Boolean(state && state.subscribers.size > 0);
  }

  // --- the driver's write surface -------------------------------------------

  function makeHandle(state) {
    return {
      deliverTranscript(messages, { snapshot = false } = {}) {
        if (snapshot) {
          state.messages = messages;
          sendSnapshot(state);
          return;
        }
        applyDiff(state, messages);
      },
      resetTranscript() {
        state.messages = [];
        state.pending = null;
        cancelFlush(state);
      },
      overlayOpen() {
        state.pending = { index: state.messages.length, broadcast: false };
      },
      overlayChanged() {
        scheduleFlush(state);
      },
      overlayResolving() {
        cancelFlush(state);
      },
      overlayDrop() {
        if (!state.pending) return;
        cancelFlush(state);
        if (state.pending.broadcast) broadcast(state, "remove", { index: state.pending.index });
        state.pending = null;
      },
      hasOverlay() {
        return state.pending !== null;
      },
      setWorking(working, { broadcast: shouldBroadcast = true } = {}) {
        state.working = working;
        if (shouldBroadcast) broadcast(state, "working", { working });
      },
      setOrchestrator(patch, { broadcast: shouldBroadcast = true } = {}) {
        state.orchestrator = { ...state.orchestrator, ...patch };
        if (shouldBroadcast) broadcast(state, "orchestrator", { orchestrator: state.orchestrator });
      },
    };
  }

  // Index-wise diff of the re-mapped transcript against the previous one.
  // Entries never reorder and ids are stable, so every difference is one of:
  // a new trailing message (append), a tool-result fold mutating one message
  // in place (replace), or — only on a branch change — a message vanishing or
  // being replaced at its index. The overlay occupies the last bubble, and an
  // append landing at its index may be its own persistence (the driver's
  // resolvesOverlay knows). Anything else emitted while the overlay lives —
  // a fold delivered late, a branch change — means the view's bubble is no
  // longer where the registry thinks it is, so the whole transcript is
  // re-snapshotted instead of patched.
  function applyDiff(state, fresh) {
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
      if (state.driver?.resolvesOverlay(entry)) {
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
        state.pending = null;
        state.driver?.onOverlayResolved?.(entry);
      } else {
        // An entry the overlay is not: a pre-run entry the watcher delivered
        // late, or (muse) a tool-call batch committed mid-run. The overlay
        // bubble was never this entry — kick it and append normally; the
        // driver re-opens the overlay on its next delta.
        if (state.pending.broadcast) broadcast(state, "remove", { index });
        broadcast(state, "append", { index, entry });
        state.pending = null;
      }
      return;
    }
    broadcast(state, "append", { index, entry });
  }

  // Coalesced overlay broadcast: deltas arrive at token frequency, but the
  // view re-renders at most FLUSH_INTERVAL_MS apart. Each flush sends the
  // fully assembled overlay (driver.overlayEntry), not a delta, so the
  // consumer's renderer never assembles anything. The index is re-pinned to
  // the transcript's current length — the position where the view holds the
  // bubble — so a watcher that lagged the live events cannot drift the
  // target. The FIRST render of a bubble goes out as an append — a replace
  // can only target a bubble the view already holds (a subscriber that
  // connected mid-run has none) — and every later flush replaces it in
  // place.
  function scheduleFlush(state) {
    if (state.flushTimer) return;
    state.flushTimer = setTimeout(() => {
      state.flushTimer = null;
      if (!state.pending) return;
      const entry = state.driver?.overlayEntry();
      if (entry) {
        const op = state.pending.broadcast ? "replace" : "append";
        state.pending.index = state.messages.length;
        state.pending.broadcast = true; // the view now holds the overlay bubble
        broadcast(state, op, { index: state.pending.index, entry });
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
    const pendingEntry = state.pending ? state.driver?.overlayEntry() : null;
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

  return { subscribe, close, hasSubscribers };
}
