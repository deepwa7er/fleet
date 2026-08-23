// bridge/test/stream.test.js
// Integration tests for the push side: boot a real bridge backed by the
// fake pi, subscribe to /session/{id}/stream, and drive it through the same
// client surface skiff uses (create, prompt_async, abort). The fake's
// scripted ordering — persist user entry, then agent_start/message_start/
// deltas/message_end, then persist the assistant entry — exercises the
// registry's append/resolve/kick rules deterministically, because the file
// writes always precede the pipe events they correspond to (the watcher
// delivers before the process events).
import { before, after, describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createBridge } from "../server.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES = path.join(HERE, "fixtures");
const FAKE_PI = path.join(FIXTURES, "fake-pi.mjs");
const FAKE_MUSE = path.join(FIXTURES, "fake-muse.mjs");
const PASSWORD = "test-password";
const PROMPT_TEXT = "hello stream hello stream hello stream hello stream";
const HEARTBEAT_MS = 100;

process.env.FAKE_PI_DELAY_MS = "80";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let bridge;
let base;
let tmp;

before(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-bridge-stream-"));
  await fs.cp(FIXTURES, tmp, { recursive: true });
  process.env.PI_CODING_AGENT_SESSION_DIR = tmp;
  bridge = createBridge({
    password: PASSWORD,
    host: "127.0.0.1",
    port: 0,
    defaultCwd: tmp,
    pi: { sessionDir: tmp, binary: FAKE_PI, maxProcesses: 8 },
    muse: { sessionDir: path.join(tmp, "muse", "sessions"), binary: FAKE_MUSE },
    opencode: { url: "http://127.0.0.1:1" },
    // Fast heartbeats so the liveness test runs in milliseconds; every other
    // test filters by event name and is indifferent to the extra frames.
    stream: { heartbeatIntervalMs: HEARTBEAT_MS },
  });
  await bridge.listen();
  base = `http://127.0.0.1:${bridge.port()}`;
});

after(async () => {
  await bridge.close();
  delete process.env.PI_CODING_AGENT_SESSION_DIR;
  await fs.rm(tmp, { recursive: true, force: true });
});

const AUTH = "Basic " + Buffer.from(`skiff:${PASSWORD}`).toString("base64");

function get(p, { auth = true } = {}) {
  return fetch(base + p, { headers: auth ? { Authorization: AUTH } : {} });
}

function post(p, body = undefined, { auth = true } = {}) {
  return fetch(base + p, {
    method: "POST",
    headers: { Authorization: AUTH, ...(body !== undefined ? { "Content-Type": "application/json" } : {}) },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
}

// --- SSE reader -------------------------------------------------------------

function parseFrame(frame) {
  let event = "message";
  const dataLines = [];
  for (const line of frame.split("\n")) {
    if (line.startsWith("event: ")) event = line.slice("event: ".length);
    else if (line.startsWith("data: ")) dataLines.push(line.slice("data: ".length));
  }
  if (dataLines.length === 0) return null;
  return { event, data: JSON.parse(dataLines.join("\n")) };
}

// Open an SSE subscription and collect frames. `next(predicate)` resolves
// with the first frame (past or future) matching the predicate.
function openStream(id) {
  return fetch(base + `/session/${id}/stream`, { headers: { Authorization: AUTH } }).then((response) => {
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("content-type"), "text/event-stream");
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    const frames = [];
    const waiters = [];
    const notify = (frame) => {
      for (let i = waiters.length - 1; i >= 0; i--) {
        if (waiters[i].predicate(frame)) {
          const [waiter] = waiters.splice(i, 1);
          clearTimeout(waiter.timer);
          waiter.resolve(frame);
        }
      }
    };
    const pump = () => {
      reader.read().then(({ done, value }) => {
        if (done) return;
        buffer += decoder.decode(value, { stream: true });
        let sep;
        while ((sep = buffer.indexOf("\n\n")) !== -1) {
          const frame = parseFrame(buffer.slice(0, sep));
          buffer = buffer.slice(sep + 2);
          if (frame) {
            frames.push(frame);
            notify(frame);
          }
        }
        pump();
      });
    };
    pump();
    return {
      frames,
      next(predicate, timeoutMs = 10000) {
        const existing = frames.find(predicate);
        if (existing) return Promise.resolve(existing);
        return new Promise((resolve, reject) => {
          const waiter = { predicate, resolve };
          waiter.timer = setTimeout(() => {
            const idx = waiters.indexOf(waiter);
            if (idx !== -1) waiters.splice(idx, 1);
            reject(new Error(`timed out waiting for a stream frame; received: ${frames.map((f) => `${f.event}${f.data.index !== undefined ? `@${f.data.index}` : ""}`).join(", ")}`));
          }, timeoutMs);
          waiters.push(waiter);
        });
      },
      async close() {
        await reader.cancel();
        await sleep(50); // let the bridge process the disconnect
      },
    };
  });
}

function snapshot(frame) {
  assert.equal(frame.event, "snapshot");
  return frame.data;
}

// A fresh session via the create flow: the process is registered with a
// session file path that does not exist until the first message persists it.
async function createSession(title = "stream test") {
  const response = await post("/session", { harness: "pi", title });
  assert.equal(response.status, 201);
  const { id } = await response.json();
  const proc = piPool().getById(id.slice("pi:".length));
  assert.ok(proc, "created session must have a registered process");
  return { id, file: proc.sessionFile };
}

function piPool() {
  return bridge.harnesses.get("pi").pool;
}

// The registry is keyed by wire session ids.
function streamKey(file) {
  return "pi:" + path.basename(file, ".jsonl");
}

// --- tests ------------------------------------------------------------------

describe("stream endpoint", () => {
  it("rejects without auth and answers 404 for an unknown session", async () => {
    const unauthorized = await fetch(base + "/session/whatever/stream");
    assert.equal(unauthorized.status, 401);

    const unknown = await get("/session/nope/stream");
    assert.equal(unknown.status, 404);
  });

  it("sends a snapshot of the file transcript on connect", async () => {
    const stream = await openStream("pi:branched");
    const first = await stream.next((f) => f.event === "snapshot");
    const payload = snapshot(first);

    // Only the current branch surfaces (5 messages; branch A is abandoned).
    assert.equal(payload.messages.length, 5);
    assert.equal(payload.messages[4].info.id, "00000009");
    assert.equal(payload.working, false);
    assert.equal(payload.orchestrator.active, false);
    assert.equal(payload.pending, null);

    await stream.close();
  });

  it("pushes the overlay growth and the resolution for a running prompt", async () => {
    const { id } = await createSession();
    const stream = await openStream(id);
    // Unborn: the session has no file yet, so the subscription is silent
    // until the first message persists it (the snapshot arrives then).

    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: PROMPT_TEXT }] });
    assert.equal(prompt.status, 200);

    // The user message may arrive as part of the initial snapshot or as an
    // append (the file appears in two writes) — wait for either, then the
    // assistant entry at index 1. That entry is a replace when the overlay
    // rendered (the pending bubble settles in place) or a plain append when
    // the overlay was kicked by the late user entry — accept both.
    await stream.next(
      (f) =>
        (f.event === "append" && f.data.index === 0 && f.data.entry.info.role === "user") ||
        (f.event === "snapshot" && f.data.messages.length === 1 && f.data.messages[0].info.role === "user")
    );

    // working flips on with agent_start.
    const workingOn = await stream.next((f) => f.event === "working" && f.data.working === true);
    assert.equal(workingOn.data.working, true);

    // The in-flight overlay streams under the <pending> id with no
    // completion time: the first render is an append (the view holds no
    // bubble yet), later coalesced flushes replace it in place.
    const firstGrow = await stream.next(
      (f) => (f.event === "append" || f.event === "replace") && f.data.entry.info.id === "<pending>"
    );
    assert.equal(firstGrow.data.entry.info.time.completed, undefined);

    // Resolution: the persisted entry replaces the overlay in place (same
    // index, real id, completed time, full text) or, if the overlay was
    // kicked, lands as a plain append.
    const resolved = await stream.next(
      (f) =>
        (f.event === "append" || f.event === "replace") &&
        f.data.entry.info.role === "assistant" &&
        f.data.entry.info.id !== "<pending>"
    );
    assert.notEqual(resolved.data.entry.info.id, "<pending>");
    assert.ok(resolved.data.entry.info.time.completed);
    assert.equal(resolved.data.entry.parts[0].text, PROMPT_TEXT);

    const workingOff = await stream.next((f) => f.event === "working" && f.data.working === false);
    assert.equal(workingOff.data.working, false);

    // The file side converged: the message endpoint sees the same 2 entries.
    const messages = await (await get(`/session/${id}/message`)).json();
    assert.equal(messages.length, 2);
    assert.equal(messages[1].parts[0].text, PROMPT_TEXT);

    await stream.close();
  });

  it("removes the overlay when a run is aborted before persisting", async () => {
    const { id } = await createSession();
    const stream = await openStream(id);

    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "abort me abort me abort me abort me abort me" }] });
    assert.equal(prompt.status, 200);

    // Wait until the overlay is visibly streaming (a delta flush landed),
    // remember its index, then abort.
    const streaming = await stream.next(
      (f) => (f.event === "append" || f.event === "replace") && f.data.entry.info.id === "<pending>"
    );
    const pendingIndex = streaming.data.index;

    const abort = await post(`/session/${id}/abort`);
    assert.equal(abort.status, 204);

    // agent_end without message_end: the overlay is removed, and the file
    // never gains the assistant entry (only the user message persisted).
    const removed = await stream.next((f) => f.event === "remove" && f.data.index === pendingIndex);
    assert.equal(removed.data.index, pendingIndex);

    const messages = await (await get(`/session/${id}/message`)).json();
    assert.equal(messages.length, 1);
    assert.equal(messages[0].info.role, "user");

    await stream.close();
  });

  it("serves a newborn session from the moment its file appears", async () => {
    const { id } = await createSession("stream newborn");
    const stream = await openStream(id);

    // No file yet: the subscription carries no state until the first message
    // persists the file, then the initial read delivers the snapshot. (The
    // heartbeat ticks regardless — it is liveness, not state.)
    await sleep(150);
    assert.deepEqual(stream.frames.filter((f) => f.event !== "heartbeat"), []);

    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "first!" }] });
    assert.equal(prompt.status, 200);

    // The user message may arrive as part of the initial snapshot or as an
    // append (the file appears in two writes) — wait for either, then the
    // assistant entry at index 1. That entry is a replace when the overlay
    // rendered (the pending bubble settles in place) or a plain append when
    // the overlay was kicked by the late user entry — accept both.
    await stream.next(
      (f) =>
        (f.event === "append" && f.data.index === 0 && f.data.entry.info.role === "user") ||
        (f.event === "snapshot" && f.data.messages.length === 1 && f.data.messages[0].info.role === "user")
    );

    const resolved = await stream.next(
      (f) =>
        (f.event === "append" || f.event === "replace") &&
        f.data.index === 1 &&
        f.data.entry.info.role === "assistant" &&
        f.data.entry.info.id !== "<pending>"
    );
    assert.equal(resolved.data.entry.parts[0].text, "first!");

    await stream.close();
  });

  it("rebroadcasts a snapshot after a compaction rewrite", async () => {
    const stream = await openStream("pi:multi-turn");
    await stream.next((f) => f.event === "snapshot");

    // Simulate compaction: the file is rewritten with a summary. A shrink
    // under the tail's offset is the detection trigger.
    const file = path.join(tmp, "multi-turn.jsonl");
    const compacted = [
      JSON.stringify({ type: "session", version: 3, id: "compact-1", timestamp: new Date().toISOString(), cwd: tmp }),
      JSON.stringify({
        type: "message",
        id: "c0000001",
        parentId: null,
        timestamp: new Date().toISOString(),
        message: { role: "assistant", content: [{ type: "text", text: "compacted summary" }], timestamp: Date.now() },
      }),
    ].join("\n") + "\n";
    await fs.writeFile(file, compacted);

    const reset = await stream.next(
      (f) =>
        f.event === "snapshot" &&
        f.data.messages.length === 1 &&
        f.data.messages[0].parts[0].text === "compacted summary"
    );
    assert.equal(reset.data.messages[0].info.id, "c0000001");

    await stream.close();
  });

  it("fans every event out to all subscribers", async () => {
    const { id } = await createSession();
    const first = await openStream(id);
    const second = await openStream(id);

    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "fan out" }] });
    assert.equal(prompt.status, 200);

    const onFirst = await first.next((f) => f.event === "working" && f.data.working === true);
    const onSecond = await second.next((f) => f.event === "working" && f.data.working === true);
    assert.deepEqual(onFirst.data, onSecond.data);

    await first.close();
    await second.close();
  });

  it("re-pins the overlay where the snapshot renders it when the pipe beats the watcher", async () => {
    const { id } = await createSession();
    const stream = await openStream(id);
    const proc = piPool().getById(id.slice("pi:".length));

    // The pipe wins the race: the assistant message starts before the file
    // watcher delivers the unborn file, so the overlay's index is stale (0)
    // when message_start lands. Events are injected through the process's
    // real line handler (which expects JSONL strings), so the overlay
    // assembly and the registry hook both fire exactly as they would from
    // pi's stdout.
    const line = (event) => proc._onLine(JSON.stringify(event));
    line({ type: "agent_start" });
    line({ type: "message_start", message: { role: "assistant", content: [] } });
    line({ type: "message_update", assistantMessageEvent: { type: "text_start", contentIndex: 0 } });
    line({ type: "message_update", assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "hel" } });

    // The file appears (header + user entry) before the first flush fires;
    // the initial read renders the snapshot with the overlay at index 1.
    const file = proc.sessionFile;
    await fs.writeFile(file, [
      JSON.stringify({ type: "session", version: 3, id: "race-1", timestamp: new Date().toISOString(), cwd: tmp }),
      JSON.stringify({
        type: "message",
        id: "race-u1",
        parentId: null,
        timestamp: new Date().toISOString(),
        message: { role: "user", content: [{ type: "text", text: "hello" }], timestamp: Date.now() },
      }),
    ].join("\n") + "\n");

    const snap = await stream.next((f) => f.event === "snapshot" && f.data.messages.length === 1);
    assert.equal(snap.data.pending.index, 1);

    // The next flush must target index 1 — where the snapshot rendered the
    // overlay — not the stale index captured at message_start.
    proc._onLine(JSON.stringify({ type: "message_update", assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "lo" } }));
    const flush = await stream.next((f) => f.event === "replace" && f.data.entry.info.id === "<pending>");
    assert.equal(flush.data.index, 1);

    // The resolution replaces the overlay in place at the same index.
    await fs.appendFile(file, JSON.stringify({
      type: "message",
      id: "race-a1",
      parentId: "race-u1",
      timestamp: new Date().toISOString(),
      message: { role: "assistant", content: [{ type: "text", text: "hello lo" }], timestamp: Date.now() },
    }) + "\n");
    const resolved = await stream.next(
      (f) => f.event === "replace" && f.data.index === 1 && f.data.entry.info.id !== "<pending>"
    );
    assert.equal(resolved.data.entry.parts[0].text, "hello lo");

    proc._onLine(JSON.stringify({ type: "agent_end", messages: [], willRetry: false }));
    proc._onLine(JSON.stringify({ type: "agent_settled" }));
    await sleep(400); // the deferred settle scan runs and finds nothing
    await stream.close();
  });

  it("unsubscribes cleanly and releases the session state", async () => {
    const { id, file } = await createSession();
    const stream = await openStream(id);
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "settle" }] });
    assert.equal(prompt.status, 200);
    await stream.next(
      (f) =>
        (f.event === "append" || f.event === "replace") &&
        f.data.entry.info.role === "assistant" &&
        f.data.entry.info.id !== "<pending>"
    );

    assert.equal(bridge.registry.hasSubscribers(streamKey(file)), true);
    await stream.close();
    assert.equal(bridge.registry.hasSubscribers(streamKey(file)), false);

    // A new subscriber starts fresh with a full snapshot.
    const again = await openStream(id);
    const snap = await again.next((f) => f.event === "snapshot");
    assert.equal(snap.data.messages.length, 2);
    await again.close();
  });

  it("ticks a heartbeat on the interval while subscribed, and stops with the last subscriber", async () => {
    const { id } = await createSession("stream heartbeat");
    const stream = await openStream(id);

    // Liveness is independent of state: ticks arrive before any file exists.
    await sleep(HEARTBEAT_MS * 3.5);
    const ticks = stream.frames.filter((f) => f.event === "heartbeat");
    assert.ok(ticks.length >= 2 && ticks.length <= 4, `expected ~3 heartbeats, got ${ticks.length}`);
    assert.deepEqual(ticks[0].data, {});

    await stream.close();
    assert.equal(bridge.registry.hasSubscribers(id), false);
    const ticksAtClose = stream.frames.filter((f) => f.event === "heartbeat").length;
    await sleep(HEARTBEAT_MS * 3);
    assert.equal(stream.frames.filter((f) => f.event === "heartbeat").length, ticksAtClose);
  });
});
