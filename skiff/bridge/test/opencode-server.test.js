// bridge/test/opencode-server.test.js
// HTTP + stream integration tests for the opencode harness: boot a real
// bridge pointed at the in-process fake serve (fixtures/fake-opencode.mjs)
// and drive it with skiff's exact client surface. The fake speaks opencode's
// own shapes, so these tests cover the adapter's whole translation layer —
// session/message mapping, child-session filtering, prompt/abort/rename
// passthrough — and the event-bus-driven stream (refetch on events, diffed
// replaces, derived working).
import { before, after, describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createBridge } from "../server.js";
import { createFakeOpencode } from "./fixtures/fake-opencode.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES = path.join(HERE, "fixtures");
const FAKE_PI = path.join(FIXTURES, "fake-pi.mjs");
const FAKE_MUSE = path.join(FIXTURES, "fake-muse.mjs");
const PASSWORD = "test-password";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let fake;
let bridge;
let base;
let tmp;
let seeded; // the pre-seeded rich session

before(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-bridge-oc-"));
  fake = createFakeOpencode({ streamDelayMs: 60 });
  await fake.listen();

  // A rich pre-seeded session: every part type the adapter maps, plus one
  // it must drop (step-start) and a child session it must filter out.
  seeded = fake.makeSession({ title: "Seeded session", directory: "/home/deepwater/code/blog" });
  fake.makeSession({ slug: "child-task", parentID: seeded.id });
  fake.messages.get(seeded.id).push(
    {
      info: { id: "msg_s1", role: "user", sessionID: seeded.id, time: { created: 1786395600000 } },
      parts: [{ id: "prt_s1", type: "text", text: "map my parts", sessionID: seeded.id, messageID: "msg_s1" }],
    },
    {
      info: {
        id: "msg_s2",
        role: "assistant",
        sessionID: seeded.id,
        modelID: "fake-oc-model",
        time: { created: 1786395601000, completed: 1786395609000 },
      },
      parts: [
        { id: "prt_s2", type: "step-start", sessionID: seeded.id, messageID: "msg_s2" },
        { id: "prt_s3", type: "reasoning", text: "thinking...", sessionID: seeded.id, messageID: "msg_s2" },
        {
          id: "prt_s4",
          type: "tool",
          callID: "call_oc_1",
          tool: "bash",
          state: { status: "completed", output: "ok", title: "ls", input: { cmd: "ls" } },
          sessionID: seeded.id,
          messageID: "msg_s2",
        },
        {
          id: "prt_s5",
          type: "tool",
          callID: "call_oc_2",
          tool: "webfetch",
          state: { status: "error", error: "connection refused", input: {} },
          sessionID: seeded.id,
          messageID: "msg_s2",
        },
        { id: "prt_s6", type: "text", text: "done", sessionID: seeded.id, messageID: "msg_s2" },
      ],
    }
  );

  bridge = createBridge({
    password: PASSWORD,
    host: "127.0.0.1",
    port: 0,
    defaultCwd: tmp,
    pi: { sessionDir: path.join(tmp, "pi-sessions"), binary: FAKE_PI, maxProcesses: 4 },
    muse: { sessionDir: path.join(tmp, "muse", "sessions"), binary: FAKE_MUSE },
    opencode: { url: fake.url() },
  });
  await bridge.listen();
  base = `http://127.0.0.1:${bridge.port()}`;
});

after(async () => {
  await bridge.close();
  await fake.close();
  await fs.rm(tmp, { recursive: true, force: true });
});

const AUTH = "Basic " + Buffer.from(`skiff:${PASSWORD}`).toString("base64");

function get(p) {
  return fetch(base + p, { headers: { Authorization: AUTH } });
}

function post(p, body = undefined, method = "POST") {
  return fetch(base + p, {
    method,
    headers: { Authorization: AUTH, ...(body !== undefined ? { "Content-Type": "application/json" } : {}) },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
}

async function until(predicate, { timeoutMs = 8000, intervalMs = 30 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await sleep(intervalMs);
  }
  return false;
}

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
            reject(new Error(`timed out; received: ${frames.map((f) => f.event).join(", ")}`));
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

describe("opencode harness HTTP", () => {
  it("lists top-level sessions tagged with harness and capabilities, no error entry", async () => {
    const { sessions, errors } = await (await get("/session")).json();
    assert.equal(errors.opencode, undefined);
    const listed = sessions.filter((s) => s.harness === "opencode");
    assert.ok(listed.some((s) => s.id === `opencode:${seeded.id}`));
    assert.ok(!listed.some((s) => s.title === "child-task" || s.id.includes("child")), "child sessions never list");
    const mine = listed.find((s) => s.id === `opencode:${seeded.id}`);
    assert.equal(mine.title, "Seeded session");
    assert.equal(mine.directory, "/home/deepwater/code/blog");
    assert.deepEqual(mine.capabilities, { rename: true, orchestrator: false });
  });

  it("falls back to the slug when a session has no title", async () => {
    const untitled = fake.makeSession({ slug: "shiny-pixel" });
    const session = await (await get(`/session/opencode:${untitled.id}`)).json();
    assert.equal(session.title, "shiny-pixel");
  });

  it("maps opencode messages to skiff's part vocabulary", async () => {
    const messages = await (await get(`/session/opencode:${seeded.id}/message`)).json();
    assert.equal(messages.length, 2);
    assert.equal(messages[0].info.role, "user");

    const assistant = messages[1];
    assert.equal(assistant.info.agent, "fake-oc-model");
    assert.equal(assistant.info.time.completed, 1786395609000);
    // step-start dropped; reasoning, both tools, and the text survive.
    assert.deepEqual(
      assistant.parts.map((p) => p.type),
      ["reasoning", "tool", "tool", "text"]
    );
    assert.deepEqual(assistant.parts[1], {
      type: "tool",
      tool: "bash",
      id: "call_oc_1",
      state: { status: "completed", output: "ok", title: "ls" },
    });
    // An errored tool surfaces its error as the output.
    assert.deepEqual(assistant.parts[2].state, { status: "error", output: "connection refused" });
  });

  it("creates and renames a session through the serve API", async () => {
    const created = await post("/session", { harness: "opencode", title: "From skiff" });
    assert.equal(created.status, 201);
    const { id } = await created.json();
    assert.match(id, /^opencode:ses_fake/);

    const renamed = await post(`/session/${id}/name`, { name: "Renamed via bridge" });
    assert.equal(renamed.status, 200);
    const session = await (await get(`/session/${id}`)).json();
    assert.equal(session.title, "Renamed via bridge");
  });

  it("rejects the orchestrator toggle (a pi capability)", async () => {
    const response = await post(`/session/opencode:${seeded.id}/orchestrator`, { on: true });
    assert.equal(response.status, 400);
    assert.match((await response.json()).error, /no orchestrator/);
  });

  it("prompts and aborts through the serve API", async () => {
    const created = await post("/session", { harness: "opencode" });
    const { id } = await created.json();
    const prompt = await post(`/session/${id}/prompt_async`, {
      parts: [{ type: "text", text: "abort this run before it finishes streaming" }],
    });
    assert.equal(prompt.status, 200);

    const abort = await post(`/session/${id}/abort`);
    assert.equal(abort.status, 204);
    // The fake completes the run on abort; the assistant message settles.
    const settled = await until(async () => {
      const messages = await (await get(`/session/${id}/message`)).json();
      const last = messages[messages.length - 1];
      return last?.info.role === "assistant" && last.info.time.completed !== undefined;
    });
    assert.ok(settled, "the aborted run never settled");
  });

  it("404s for unknown opencode sessions", async () => {
    assert.equal((await get("/session/opencode:ses_nope")).status, 404);
    assert.equal((await post("/session/opencode:ses_nope/prompt_async", { parts: [{ type: "text", text: "x" }] })).status, 404);
  });
});

describe("opencode harness stream", () => {
  it("streams a run: snapshot, growth replaces, derived working on and off", async () => {
    const created = await post("/session", { harness: "opencode" });
    const { id } = await created.json();
    const stream = await openStream(id);

    // Connect: an immediate snapshot of the (empty) transcript.
    const first = await stream.next((f) => f.event === "snapshot");
    assert.deepEqual(first.data.messages, []);
    assert.equal(first.data.working, false);

    const text = "stream through the event bus stream through the event bus";
    await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text }] });

    // The refetch appends both messages and derives working from the
    // incomplete assistant message.
    await stream.next((f) => f.event === "working" && f.data.working === true);
    const grow = await stream.next(
      (f) =>
        f.event === "replace" &&
        f.data.entry.info.role === "assistant" &&
        f.data.entry.parts[0]?.text.length > 0 &&
        f.data.entry.info.time.completed === undefined
    );
    assert.ok(grow.data.entry.parts[0].text.length < text.length, "growth must be observed mid-stream");

    const done = await stream.next(
      (f) => f.event === "replace" && f.data.entry.info.time?.completed !== undefined && f.data.entry.parts[0]?.text === text
    );
    assert.ok(done);
    await stream.next((f) => f.event === "working" && f.data.working === false);

    await stream.close();
  });

  it("404s the stream for unknown sessions", async () => {
    const response = await fetch(base + "/session/opencode:ses_missing/stream", { headers: { Authorization: AUTH } });
    assert.equal(response.status, 404);
  });
});
