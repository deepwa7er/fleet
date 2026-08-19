// bridge/test/muse-server.test.js
// HTTP + stream integration tests for the muse harness: boot a real bridge
// whose muse binary is the scripted fake (fixtures/fake-muse.mjs), and drive
// it with skiff's exact client surface. The fake mirrors real muse's
// observed behavior — the session dir resolves via XDG_DATA_HOME, the first
// run creates the dated session directory, stdout carries incremental
// output deltas while committed messages land in the file only, and SIGINT
// kills a run without a terminal record — so these tests exercise the same
// unborn-directory, overlay-resolution, and exit-convergence paths real muse
// does.
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
const FIXTURE_ID = "muse:26ea1b5e-0000-4000-8000-0000000000f1";

process.env.FAKE_MUSE_DELAY_MS = "80";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let bridge;
let base;
let tmp;
let museSessions;

before(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-bridge-muse-"));
  museSessions = path.join(tmp, "muse", "sessions");
  await fs.cp(path.join(FIXTURES, "muse-sessions"), museSessions, { recursive: true });
  bridge = createBridge({
    password: PASSWORD,
    host: "127.0.0.1",
    port: 0,
    defaultCwd: tmp,
    pi: { sessionDir: path.join(tmp, "pi-sessions"), binary: FAKE_PI, maxProcesses: 4 },
    muse: { sessionDir: museSessions, binary: FAKE_MUSE },
    opencode: { url: "http://127.0.0.1:1" },
  });
  await bridge.listen();
  base = `http://127.0.0.1:${bridge.port()}`;
});

after(async () => {
  await bridge.close();
  await fs.rm(tmp, { recursive: true, force: true });
});

const AUTH = "Basic " + Buffer.from(`skiff:${PASSWORD}`).toString("base64");

function get(p) {
  return fetch(base + p, { headers: { Authorization: AUTH } });
}

function post(p, body = undefined) {
  return fetch(base + p, {
    method: "POST",
    headers: { Authorization: AUTH, ...(body !== undefined ? { "Content-Type": "application/json" } : {}) },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
}

async function messages(id) {
  const response = await get(`/session/${id}/message`);
  assert.equal(response.status, 200);
  return response.json();
}

async function until(predicate, { timeoutMs = 8000, intervalMs = 30 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await sleep(intervalMs);
  }
  return false;
}

// --- SSE reader (same shape as stream.test.js) ------------------------------

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

function openStream(id) {
  return fetch(base + `/session/${id}/stream`, { headers: { Authorization: AUTH } }).then((response) => {
    assert.equal(response.status, 200);
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
            reject(new Error(`timed out; received: ${frames.map((f) => `${f.event}${f.data.index !== undefined ? `@${f.data.index}` : ""}`).join(", ")}`));
          }, timeoutMs);
          waiters.push(waiter);
        });
      },
      async close() {
        await reader.cancel();
        await sleep(50);
      },
    };
  });
}

// --- tests ------------------------------------------------------------------

describe("muse harness HTTP", () => {
  it("lists the fixture session, tagged with harness and capabilities", async () => {
    const { sessions } = await (await get("/session")).json();
    const fixture = sessions.find((s) => s.id === FIXTURE_ID);
    assert.ok(fixture, "the muse fixture session must be listed");
    assert.equal(fixture.harness, "muse");
    assert.equal(fixture.title, "lemon-aurora");
    assert.deepEqual(fixture.capabilities, { rename: false, orchestrator: false });
  });

  it("serves the fixture transcript with folded tool results", async () => {
    const transcript = await messages(FIXTURE_ID);
    assert.equal(transcript.length, 3);
    assert.equal(transcript[0].info.role, "user");
    assert.equal(transcript[1].parts[0].state.status, "completed");
    assert.equal(transcript[2].parts[0].text, "The muse store maps the event log to a transcript.");
  });

  it("rejects rename and orchestrator (capabilities the harness lacks)", async () => {
    const rename = await post(`/session/${FIXTURE_ID}/name`, { name: "nope" });
    assert.equal(rename.status, 400);
    assert.match((await rename.json()).error, /cannot be renamed/);

    const orch = await post(`/session/${FIXTURE_ID}/orchestrator`, { on: true });
    assert.equal(orch.status, 400);
    assert.match((await orch.json()).error, /no orchestrator/);
  });

  it("creates a newborn session, keeps it visible, and persists it on first prompt", async () => {
    const created = await post("/session", { harness: "muse", title: "ignored" });
    assert.equal(created.status, 201);
    const { id } = await created.json();
    assert.match(id, /^muse:[0-9a-f-]{36}$/);

    // Newborn: no file, no directory on disk — served from the runner.
    const show = await get(`/session/${id}`);
    assert.equal(show.status, 200);
    const newborn = await show.json();
    assert.equal(newborn.title, null);
    assert.deepEqual(await messages(id), []);
    const listed = await (await get("/session")).json();
    assert.ok(listed.sessions.some((s) => s.id === id), "newborn sessions are listed");

    // First prompt: muse creates the dated session dir and streams; the
    // committed reply (the fake echoes the prompt) lands in the file.
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "hello muse bridge" }] });
    assert.equal(prompt.status, 200);
    const done = await until(async () => {
      const transcript = await messages(id);
      const last = transcript[transcript.length - 1];
      return last?.info.role === "assistant" && last.info.time?.completed !== undefined && last.parts[0]?.text === "hello muse bridge";
    });
    assert.ok(done, "the committed assistant reply never appeared");

    // The file-backed session now carries muse's automatic name.
    const persisted = await (await get(`/session/${id}`)).json();
    assert.equal(persisted.title, `fake-${id.slice("muse:".length, "muse:".length + 8)}`);
  });

  it("reports busy while a run streams and rejects a concurrent prompt", async () => {
    const created = await post("/session", { harness: "muse" });
    const { id } = await created.json();
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "busy probe running long enough" }] });
    assert.equal(prompt.status, 200);

    const sawBusy = await until(async () => {
      const statuses = await (await get("/session/status")).json();
      return statuses[id]?.type === "busy";
    });
    assert.ok(sawBusy, "status never showed busy while the fake streamed");

    const second = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "too eager" }] });
    assert.equal(second.status, 409);

    const sawIdle = await until(async () => {
      const statuses = await (await get("/session/status")).json();
      return statuses[id] === undefined;
    });
    assert.ok(sawIdle, "status never cleared after the run finished");
  });

  it("surfaces a refused run as 502 with muse's stderr", async () => {
    process.env.FAKE_MUSE_FAIL = "1";
    try {
      const created = await post("/session", { harness: "muse" });
      const { id } = await created.json();
      const response = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "doomed" }] });
      assert.equal(response.status, 502);
      assert.match((await response.json()).error, /refusing to run/);
    } finally {
      delete process.env.FAKE_MUSE_FAIL;
    }
  });

  it("404s prompt and abort for unknown muse sessions", async () => {
    const prompt = await post("/session/muse:00000000-0000-4000-8000-000000000000/prompt_async", {
      parts: [{ type: "text", text: "hi" }],
    });
    assert.equal(prompt.status, 404);
    const abort = await post("/session/muse:00000000-0000-4000-8000-000000000000/abort");
    assert.equal(abort.status, 404);
  });
});

describe("muse harness stream", () => {
  it("streams a run end to end: unborn dir, overlay growth, resolution, working", async () => {
    const created = await post("/session", { harness: "muse" });
    const { id } = await created.json();
    const stream = await openStream(id);

    // Unborn: not even the session directory exists yet; the subscription
    // is silent until the first run persists the file.
    await sleep(150);
    assert.equal(stream.frames.length, 0);

    const text = "stream this muse reply stream this muse reply";
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text }] });
    assert.equal(prompt.status, 200);

    const workingOn = await stream.next((f) => f.event === "working" && f.data.working === true);
    assert.equal(workingOn.data.working, true);

    // The user message arrives via the initial snapshot or as an append,
    // depending on how the watcher races the file creation.
    await stream.next(
      (f) =>
        (f.event === "append" && f.data.entry.info.role === "user") ||
        (f.event === "snapshot" && f.data.messages.some((m) => m.info.role === "user"))
    );

    // Overlay growth: the accumulated output streams under <pending>.
    const grow = await stream.next(
      (f) => (f.event === "append" || f.event === "replace") && f.data.entry.info.id === "<pending>"
    );
    assert.equal(grow.data.entry.info.time.completed, undefined);

    // Resolution: the committed entry replaces (or lands in place of) the
    // overlay with the full echoed text.
    const resolved = await stream.next(
      (f) =>
        (f.event === "append" || f.event === "replace") &&
        f.data.entry.info.role === "assistant" &&
        f.data.entry.info.id !== "<pending>" &&
        f.data.entry.parts[0]?.text === text
    );
    assert.ok(resolved.data.entry.info.time.completed);

    await stream.next((f) => f.event === "working" && f.data.working === false);

    // The poll side converged on the same transcript.
    const transcript = await messages(id);
    assert.equal(transcript.filter((m) => m.info.id === "<pending>").length, 0);
    assert.equal(transcript[transcript.length - 1].parts[0].text, text);

    await stream.close();
  });

  it("commits tool traffic mid-run without breaking the overlay", async () => {
    process.env.FAKE_MUSE_TOOLS = "1";
    try {
      const created = await post("/session", { harness: "muse" });
      const { id } = await created.json();
      const stream = await openStream(id);

      const text = "tools then text tools then text";
      await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text }] });

      // The tool batch commits to the file mid-run and surfaces as its own
      // assistant bubble (folded to completed by the result batch), while
      // the streamed text still resolves afterwards.
      const toolMessage = await stream.next(
        (f) =>
          (f.event === "append" || f.event === "replace" || f.event === "snapshot") &&
          JSON.stringify(f.data).includes('"tool":"bash"')
      );
      assert.ok(toolMessage);
      const resolved = await stream.next(
        (f) =>
          (f.event === "append" || f.event === "replace") &&
          f.data.entry?.info.role === "assistant" &&
          f.data.entry.info.id !== "<pending>" &&
          f.data.entry.parts[0]?.text === text
      );
      assert.ok(resolved);
      await stream.next((f) => f.event === "working" && f.data.working === false);

      const transcript = await messages(id);
      const toolPart = transcript.flatMap((m) => m.parts).find((p) => p.type === "tool");
      assert.deepEqual(toolPart.state, { status: "completed", output: "tool output" });

      await stream.close();
    } finally {
      delete process.env.FAKE_MUSE_TOOLS;
    }
  });

  it("aborting a run drops the overlay and settles working, and the session recovers", async () => {
    const created = await post("/session", { harness: "muse" });
    const { id } = await created.json();
    const stream = await openStream(id);

    const prompt = await post(`/session/${id}/prompt_async`, {
      parts: [{ type: "text", text: "abort me abort me abort me abort me abort me" }],
    });
    assert.equal(prompt.status, 200);

    // Wait until the overlay is visibly streaming, then abort. The fake (like
    // real muse) dies on SIGINT without a terminal record; the exit handler
    // owns convergence.
    await stream.next((f) => (f.event === "append" || f.event === "replace") && f.data.entry.info.id === "<pending>");
    const abort = await post(`/session/${id}/abort`);
    assert.equal(abort.status, 204);

    await stream.next((f) => f.event === "working" && f.data.working === false);
    const settled = await until(async () => {
      const transcript = await messages(id);
      return !transcript.some((m) => m.info.id === "<pending>");
    });
    assert.ok(settled, "the aborted overlay never settled out of the transcript");

    // The session is not wedged: a later prompt runs to completion.
    const again = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "recovered" }] });
    assert.equal(again.status, 200);
    const recovered = await until(async () => {
      const transcript = await messages(id);
      const last = transcript[transcript.length - 1];
      return last?.parts[0]?.text === "recovered" && last.info.time?.completed !== undefined;
    });
    assert.ok(recovered, "the session never recovered after the abort");

    await stream.close();
  });
});
