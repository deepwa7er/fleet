// bridge/test/server.test.js
// HTTP integration tests: boot a real bridge on an ephemeral port backed by a
// temp session dir, and drive it with skiff's exact client surface. The fake
// pi binary (fixtures/fake-pi.mjs) stands in for `pi --mode rpc`.
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
const PASSWORD = "test-password";

// Slower per-delta streaming makes the "text grew" and busy assertions
// reliable on loaded CI machines; total stream is ~400ms.
process.env.FAKE_PI_DELAY_MS = "120";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let bridge;
let base;
let tmp;
let fixtures;

before(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-bridge-http-"));
  await fs.cp(FIXTURES, tmp, { recursive: true });
  // The default deployment spawns pi WITHOUT --session-dir, so the spawned
  // fake must resolve its storage the way real pi does: PI_CODING_AGENT_
  // SESSION_DIR, which the bridge's children inherit. Pointing it at the
  // scan dir makes the whole default path loop through the real file
  // round-trip (newborn -> first message -> on-disk session).
  process.env.PI_CODING_AGENT_SESSION_DIR = tmp;
  bridge = createBridge({
    password: PASSWORD,
    host: "127.0.0.1",
    port: 0,
    sessionDir: tmp,
    binary: FAKE_PI,
    defaultCwd: tmp,
    maxProcesses: 8,
  });
  await bridge.listen();
  base = `http://127.0.0.1:${bridge.port()}`;
  fixtures = {
    multiTurn: "multi-turn",
    branched: "branched",
    trailingPartial: "trailing-partial",
    empty: "empty",
  };
});

after(async () => {
  await bridge.close();
  delete process.env.PI_CODING_AGENT_SESSION_DIR;
  await fs.rm(tmp, { recursive: true, force: true });
});

const AUTH = "Basic " + Buffer.from(`opencode:${PASSWORD}`).toString("base64");

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

async function lastMessage(id) {
  const response = await get(`/session/${id}/message`);
  assert.equal(response.status, 200);
  const messages = await response.json();
  return messages[messages.length - 1] ?? null;
}

// Poll a predicate until it is true or the deadline passes.
async function until(predicate, { timeoutMs = 5000, intervalMs = 30 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await sleep(intervalMs);
  }
  return false;
}

describe("bridge HTTP", () => {
  it("rejects requests without basic auth", async () => {
    const response = await get("/session", { auth: false });
    assert.equal(response.status, 401);
  });

  it("serves /global/health", async () => {
    const response = await get("/global/health");
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { status: "ok" });
  });

  it("lists sessions", async () => {
    const response = await get("/session");
    assert.equal(response.status, 200);
    const sessions = await response.json();
    assert.equal(sessions.length, 4);
    const multi = sessions.find((s) => s.id === "multi-turn");
    assert.equal(multi.title, "Multi-turn fixture");
    assert.equal(multi.directory, "/home/deepwater/code/skiff");
    assert.deepEqual(multi.model, { id: "deepseek-v4-flash" });
  });

  it("shows one session and 404s for unknown ids", async () => {
    const response = await get(`/session/${fixtures.multiTurn}`);
    assert.equal(response.status, 200);
    const session = await response.json();
    assert.equal(session.id, "multi-turn");

    const missing = await get("/session/does-not-exist");
    assert.equal(missing.status, 404);
  });

  it("serves messages with tool pairing", async () => {
    const response = await get(`/session/${fixtures.multiTurn}/message`);
    assert.equal(response.status, 200);
    const messages = await response.json();
    assert.equal(messages.length, 5); // the toolResult folds into the tool part
    assert.equal(messages[1].parts[2].type, "tool");
    assert.equal(messages[1].parts[2].state.status, "completed");
  });

  it("serves an empty transcript for an empty session", async () => {
    const response = await get(`/session/${fixtures.empty}/message`);
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), []);
  });

  it("404s messages for unknown sessions", async () => {
    const response = await get("/session/does-not-exist/message");
    assert.equal(response.status, 404);
  });

  it("creates a session (newborn: served from process state until its file appears)", async () => {
    const response = await post("/session", { title: "Created by test" });
    assert.equal(response.status, 201);
    const { id } = await response.json();
    assert.match(id, /^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z_[0-9a-f-]{36}$/);

    // the file does not exist yet (real pi only persists on the first
    // message), so the show/message routes must serve the newborn session
    const show = await get(`/session/${id}`);
    assert.equal(show.status, 200);
    const session = await show.json();
    assert.equal(session.id, id);
    assert.equal(session.title, "Created by test");

    const messages = await get(`/session/${id}/message`);
    assert.deepEqual(await messages.json(), []);

    // first prompt persists the file; the session then reads from disk
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "first message" }] });
    assert.equal(prompt.status, 200);
    const done = await until(async () => {
      const last = await lastMessage(id);
      return last && last.info.role === "assistant" && last.info.time?.completed !== undefined;
    });
    assert.ok(done, "the created session's first message never completed");

    const listing = await get("/session");
    const sessions = await listing.json();
    assert.ok(sessions.some((s) => s.id === id), "created session is listed once its file exists");
  });

  it("streams a prompt: overlay text grows, then the completed entry lands on disk", async () => {
    const id = fixtures.multiTurn;
    const promptText = "Hello bridge, stream this message in parts";
    const response = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: promptText }] });
    assert.equal(response.status, 200);

    // capture the first observed in-flight (pending) state
    let partial = null;
    const sawPending = await until(async () => {
      const last = await lastMessage(id);
      if (last?.info.id === "<pending>") {
        partial = last;
        return true;
      }
      return last?.info.role === "assistant" && last.info.time?.completed !== undefined;
    });
    assert.ok(sawPending, "the stream ended before any poll could observe it");

    if (partial) {
      assert.equal(partial.info.role, "assistant");
      assert.equal(partial.info.time.completed, undefined);
      assert.ok(partial.parts[0].text.length > 0, "the overlay should already carry streamed text");
    }

    // ...and the completed entry lands on disk afterwards
    const completed = await until(async () => {
      const last = await lastMessage(id);
      return last?.info.id !== "<pending>" && last?.info.role === "assistant" && last.info.time?.completed !== undefined;
    });
    assert.ok(completed, "the completed assistant message never appeared");
    const final = await lastMessage(id);
    assert.equal(final.parts[0].text, promptText);
    if (partial) {
      assert.ok(final.parts[0].text.startsWith(partial.parts[0].text), "streamed text must only grow");
      assert.ok(final.parts[0].text.length > partial.parts[0].text.length, "streamed text must have grown");
    }
  });

  it("passes --session-dir only when the session dir is explicitly overridden", async () => {
    const argvFile = path.join(tmp, "spawn-argv.jsonl");
    process.env.FAKE_PI_ARGV_FILE = argvFile;
    try {
      // Default deployment: no PI_SESSION_DIR, so spawned pi must NOT get
      // --session-dir (pi's native per-cwd layout is what the CLI shares).
      // trailingPartial is untouched by earlier tests, so this prompt forces
      // a fresh spawn (a pooled process would not re-record its argv).
      const id = fixtures.trailingPartial;
      await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "argv probe (default path)" }] });
      await until(async () => (await lastMessage(id))?.info.time?.completed !== undefined);
      const defaultArgv = JSON.parse((await fs.readFile(argvFile, "utf8")).trim().split("\n")[0]);
      assert.ok(!defaultArgv.includes("--session-dir"), `default spawn must not pass --session-dir: ${defaultArgv}`);

      // Explicit override: the pool must pass --session-dir so scanning and
      // writing stay in one place.
      await fs.rm(argvFile, { force: true });
      const explicit = createBridge({
        password: PASSWORD,
        host: "127.0.0.1",
        port: 0,
        sessionDir: tmp,
        sessionDirExplicit: true,
        binary: FAKE_PI,
        defaultCwd: tmp,
        maxProcesses: 2,
      });
      try {
        await explicit.listen();
        const explicitBase = `http://127.0.0.1:${explicit.port()}`;
        const auth = { Authorization: AUTH };
        await fetch(`${explicitBase}/session/${fixtures.trailingPartial}/prompt_async`, {
          method: "POST",
          headers: { ...auth, "Content-Type": "application/json" },
          body: JSON.stringify({ parts: [{ type: "text", text: "argv probe (explicit path)" }] }),
        });
        await until(async () => {
          const r = await fetch(`${explicitBase}/session/${fixtures.trailingPartial}/message`, { headers: auth });
          const messages = await r.json();
          return messages.at(-1)?.info.time?.completed !== undefined;
        });
        const explicitArgv = JSON.parse((await fs.readFile(argvFile, "utf8")).trim().split("\n")[0]);
        assert.ok(explicitArgv.includes("--session-dir"), `explicit spawn must pass --session-dir: ${explicitArgv}`);
        assert.ok(explicitArgv.includes(tmp), "--session-dir must point at the scan dir");
      } finally {
        await explicit.close();
      }
    } finally {
      delete process.env.FAKE_PI_ARGV_FILE;
      await fs.rm(argvFile, { force: true });
    }
  });

  it("reports busy while a run streams and clears after agent_settled", async () => {
    const id = fixtures.branched;
    const response = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "status probe message" }] });
    assert.equal(response.status, 200);

    const sawBusy = await until(async () => {
      const statuses = await (await get("/session/status")).json();
      return statuses[id]?.type === "busy";
    });
    assert.ok(sawBusy, "status never showed busy while the fake streamed");

    const sawIdle = await until(async () => {
      const statuses = await (await get("/session/status")).json();
      return Object.keys(statuses).length === 0;
    });
    assert.ok(sawIdle, "status never cleared after agent_settled");
  });

  it("toggles orchestrator mode through the live process and serves it back", async () => {
    const created = await post("/session", { title: "orch toggle" });
    assert.equal(created.status, 201);
    const id = (await created.json()).id;

    const on = await post(`/session/${id}/orchestrator`, { on: true });
    assert.equal(on.status, 200);
    assert.deepEqual(await on.json(), { ok: true });

    // The fake pi mirrors the extension: the toggle lands as an
    // "orchestrator-mode" custom entry. The session is newborn (no file
    // yet), and real pi buffers the entry in memory until the first
    // assistant message — so the mode must be served from the process's
    // entry_appended record, not from a file.
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return session.orchestrator?.active === true;
      }),
      "mode never flipped to on (newborn, process-served)"
    );

    const off = await post(`/session/${id}/orchestrator`, { on: false });
    assert.equal(off.status, 200);
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return session.orchestrator?.active === false;
      }),
      "mode never flipped back to off"
    );

    // Toggle back on, then send the first message: the buffered entry must
    // flush with the file, so the mode stays visible once the session
    // reads from disk.
    await post(`/session/${id}/orchestrator`, { on: true });
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "first message" }] });
    assert.equal(prompt.status, 200);
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return session.orchestrator?.active === true;
      }),
      "mode lost when the first message flushed the file"
    );

    // The toggle is not a message: the transcript never mentions it, and
    // the session list carries the mode like the show route does.
    const messages = await (await get(`/session/${id}/message`)).json();
    assert.equal(messages.filter((m) => m.info.role === "user").length, 1);
    assert.equal(messages.filter((m) => m.info.role === "assistant").length, 1);
    const sessions = await (await get("/session")).json();
    assert.equal(sessions.find((s) => s.id === id).orchestrator.active, true);
  });

  it("serves the orchestrator extension's live widget and status with the session", async () => {
    const created = await post("/session", { title: "orch widget" });
    assert.equal(created.status, 201);
    const id = (await created.json()).id;

    // Toggle on: the fake mirrors the extension's updateWidget — a
    // fire-and-forget setWidget/setStatus publication with the live plan
    // readout. The bridge captures it and serves it with the session object
    // (newborn window: no file yet).
    const on = await post(`/session/${id}/orchestrator`, { on: true });
    assert.equal(on.status, 200);
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return (
          session.orchestrator?.active === true &&
          Array.isArray(session.orchestrator.widget) &&
          session.orchestrator.widget.some((line) => line.includes("fake plan")) &&
          session.orchestrator.status === "orchestrator: planned · 0/2 steps done"
        );
      }),
      "widget/status never appeared after toggle-on"
    );

    // Toggle off: the extension clears the readout, so the fields drop from
    // the session object.
    const off = await post(`/session/${id}/orchestrator`, { on: false });
    assert.equal(off.status, 200);
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return session.orchestrator?.active === false && session.orchestrator?.widget === undefined;
      }),
      "widget never cleared after toggle-off"
    );

    // Back on with a persisted file: the live process still serves the
    // readout on top of the file's mode record.
    await post(`/session/${id}/orchestrator`, { on: true });
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "first message" }] });
    assert.equal(prompt.status, 200);
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return session.orchestrator?.active === true && session.orchestrator?.widget?.length === 3;
      }),
      "widget lost once the session reads from disk"
    );
  });

  it("rejects an invalid orchestrator toggle body and 404s unknown sessions", async () => {
    const bad = await post(`/session/${fixtures.multiTurn}/orchestrator`, { on: "yes" });
    assert.equal(bad.status, 400);
    const missing = await post("/session/does-not-exist/orchestrator", { on: true });
    assert.equal(missing.status, 404);
  });

  it("502s when pi rejects the orchestrator toggle", async () => {
    process.env.FAKE_PI_PROMPT_ERROR = "1";
    try {
      // Create a fresh session while the failure env is set, so its live
      // process inherits it (pooled processes keep their spawn-time env;
      // no other test touches this session).
      const created = await post("/session", { title: "rejected toggle" });
      const id = (await created.json()).id;
      const response = await post(`/session/${id}/orchestrator`, { on: true });
      assert.equal(response.status, 502);
      const body = await response.json();
      assert.match(body.error, /orchestrator toggle failed/);
    } finally {
      delete process.env.FAKE_PI_PROMPT_ERROR;
    }
  });

  it("renames a file-backed session through its live process and serves the new title", async () => {
    const id = fixtures.multiTurn;
    const before = await (await get(`/session/${id}/message`)).json();

    // The name is trimmed on the wire, like the create flow trims its title.
    const response = await post(`/session/${id}/name`, { name: "  Renamed fixture  " });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true });

    // pi persisted a session_info entry; the file reader serves the new title.
    assert.ok(
      await until(async () => {
        const session = await (await get(`/session/${id}`)).json();
        return session.title === "Renamed fixture";
      }),
      "session title never updated"
    );

    // The rename is not a message: the transcript is untouched.
    const after = await (await get(`/session/${id}/message`)).json();
    assert.equal(after.length, before.length);
  });

  it("renames a newborn session and keeps the title across its first message", async () => {
    const created = await post("/session", { title: "before rename" });
    assert.equal(created.status, 201);
    const id = (await created.json()).id;

    const renamed = await post(`/session/${id}/name`, { name: "after rename" });
    assert.equal(renamed.status, 200);

    // No file yet: the title is served from the process state.
    const newborn = await (await get(`/session/${id}`)).json();
    assert.equal(newborn.title, "after rename");

    // The first message persists the file, header + the renamed session_info
    // entry, so the file-derived title matches the renamed one.
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "hello" }] });
    assert.equal(prompt.status, 200);
    const done = await until(async () => {
      const last = await lastMessage(id);
      return last && last.info.role === "assistant" && last.info.time?.completed !== undefined;
    });
    assert.ok(done, "the renamed session's first message never completed");

    const fromFile = await (await get(`/session/${id}`)).json();
    assert.equal(fromFile.title, "after rename");
  });

  it("rejects an empty rename name and 404s unknown sessions", async () => {
    const bad = await post(`/session/${fixtures.multiTurn}/name`, { name: "   " });
    assert.equal(bad.status, 400);
    const missing = await post("/session/does-not-exist/name", { name: "x" });
    assert.equal(missing.status, 404);
  });

  it("502s when pi rejects the rename", async () => {
    process.env.FAKE_PI_SET_NAME_ERROR = "1";
    try {
      // Create a fresh session while the failure env is set, so its live
      // process inherits it (pooled processes keep their spawn-time env;
      // no other test touches this session). The failure hook is
      // name-conditional — the create flow's own set_session_name (with the
      // session title) must still succeed, so the title avoids the marker
      // and only the rename trips the rejection.
      const created = await post("/session", { title: "rename failure" });
      assert.equal(created.status, 201);
      const id = (await created.json()).id;
      const response = await post(`/session/${id}/name`, { name: "rejected name" });
      assert.equal(response.status, 502);
      const body = await response.json();
      assert.match(body.error, /rename failed/);
    } finally {
      delete process.env.FAKE_PI_SET_NAME_ERROR;
    }
  });

  it("aborts a running prompt: 204, status clears, no partial lands on disk", async () => {
    const id = fixtures.empty;
    const prompt = await post(`/session/${id}/prompt_async`, { parts: [{ type: "text", text: "abort me now please" }] });
    assert.equal(prompt.status, 200);
    await sleep(60); // let streaming start
    const abort = await post(`/session/${id}/abort`);
    assert.equal(abort.status, 204);

    const cleared = await until(async () => {
      const statuses = await (await get("/session/status")).json();
      return Object.keys(statuses).length === 0;
    });
    assert.ok(cleared, "status never cleared after abort");

    const messages = await (await get(`/session/${id}/message`)).json();
    assert.ok(!messages.some((m) => m.info.id === "<pending>"), "aborted partial must not linger in the overlay");
    assert.ok(!messages.some((m) => m.info.role === "assistant"), "aborted text must not land on disk");
  });

  it("404s prompt and abort for unknown sessions", async () => {
    const prompt = await post("/session/does-not-exist/prompt_async", { parts: [{ type: "text", text: "hi" }] });
    assert.equal(prompt.status, 404);
    const abort = await post("/session/does-not-exist/abort");
    assert.equal(abort.status, 404);
  });

  it("rejects a prompt with no text and one with unsupported parts", async () => {
    const noText = await post(`/session/${fixtures.multiTurn}/prompt_async`, { parts: [] });
    assert.equal(noText.status, 400);
    const image = await post(`/session/${fixtures.multiTurn}/prompt_async`, {
      parts: [{ type: "image", data: "AAAA" }],
    });
    assert.equal(image.status, 400);
  });

  it("rejects malformed JSON bodies", async () => {
    const response = await fetch(base + `/session/${fixtures.multiTurn}/prompt_async`, {
      method: "POST",
      headers: { Authorization: AUTH, "Content-Type": "application/json" },
      body: "{not json",
    });
    assert.equal(response.status, 400);
  });
});
