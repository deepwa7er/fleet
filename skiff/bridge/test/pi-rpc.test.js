// bridge/test/pi-rpc.test.js
// Unit tests for the RPC framing, event assembly, busy tracking, extension-UI
// auto-cancel, and the pool's LRU eviction.
import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
import { createJsonlReader, PiProcess, PiPool } from "../lib/pi-rpc.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FAKE_PI = path.join(HERE, "fixtures", "fake-pi.mjs");
const FIXTURES = path.join(HERE, "fixtures");

// A PiProcess that never spawns: _onLine is fed directly, so the streaming
// assembly and busy bookkeeping can be tested deterministically. _onLine
// consumes JSONL strings (its real contract), so feeds are stringified.
function offlineProcess() {
  return new PiProcess({ binary: FAKE_PI, sessionFile: null, cwd: "/tmp", sessionDir: "/tmp" });
}

function feed(proc, event) {
  proc._onLine(JSON.stringify(event));
}

describe("createJsonlReader", () => {
  it("splits on LF only and tolerates CRLF and U+2028 inside JSON", async () => {
    const lines = [];
    const ended = new Promise((resolve) => {
      const stream = Readable.from([
        Buffer.from('{"a":"x\u2028y"}\r\n'),
        Buffer.from('{"b":2}\n'),
        Buffer.from('{"c":3}'), // no trailing newline
      ]);
      createJsonlReader(stream, (line) => lines.push(line), resolve);
    });
    await ended;
    assert.deepEqual(lines, ['{"a":"x\u2028y"}', '{"b":2}', '{"c":3}']);
  });
});

describe("PiProcess streaming assembly", () => {
  it("assembles a partial assistant message from deltas without time.completed", () => {
    const proc = offlineProcess();
    feed(proc, { type: "agent_start" });
    feed(proc, { type: "message_start", message: { role: "assistant", content: [], model: "deepseek-v4-flash" } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "text_start", contentIndex: 0 } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "Hello " } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "world" } });

    const pending = proc.getPendingMessage();
    assert.equal(pending.info.id, "<pending>");
    assert.equal(pending.info.role, "assistant");
    assert.equal(pending.info.agent, "deepseek-v4-flash");
    assert.equal(pending.info.time.completed, undefined);
    assert.deepEqual(pending.parts, [{ type: "text", text: "Hello world", id: "<pending>-p0" }]);
  });

  it("handles thinking and toolcall blocks by contentIndex, with toolcall_end authoritative", () => {
    const proc = offlineProcess();
    feed(proc, { type: "message_start", message: { role: "assistant", content: [], model: null } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "thinking_start", contentIndex: 0 } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "thinking_delta", contentIndex: 0, delta: "hmm" } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "toolcall_start", contentIndex: 1, toolCall: { type: "toolCall", id: "call_x", name: "bash" } } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "toolcall_delta", contentIndex: 1, delta: '{"command":"ls"}' } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "toolcall_end", contentIndex: 1, toolCall: { type: "toolCall", id: "call_x", name: "bash", arguments: { command: "ls" } } } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "thinking_end", contentIndex: 0, content: "hmm" } });

    const pending = proc.getPendingMessage();
    assert.deepEqual(pending.parts.map((p) => p.type), ["reasoning", "tool"]);
    assert.equal(pending.parts[0].text, "hmm");
    assert.equal(pending.parts[1].tool, "bash");
    assert.equal(pending.parts[1].id, "call_x");
    assert.deepEqual(pending.parts[1].state, { status: "running" });
  });

  it("ignores non-assistant message streams (user messages are persisted)", () => {
    const proc = offlineProcess();
    feed(proc, { type: "message_start", message: { role: "user", content: [] } });
    assert.equal(proc.getPendingMessage(), null);
  });

  it("drops the overlay at message_end (the entry lands on disk)", () => {
    const proc = offlineProcess();
    feed(proc, { type: "message_start", message: { role: "assistant", content: [], model: null } });
    feed(proc, { type: "message_end", message: { role: "assistant", content: [{ type: "text", text: "done" }] } });
    assert.equal(proc.getPendingMessage(), null);
  });

  it("drops an abandoned partial on agent_end (aborted run never persists)", () => {
    const proc = offlineProcess();
    feed(proc, { type: "message_start", message: { role: "assistant", content: [], model: null } });
    feed(proc, { type: "message_update", assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "partial" } });
    feed(proc, { type: "agent_end", messages: [], willRetry: false });
    assert.equal(proc.getPendingMessage(), null);
  });
});

describe("PiProcess busy tracking", () => {
  it("is busy from agent_start until agent_settled", () => {
    const proc = offlineProcess();
    assert.equal(proc.isBusy(), false); // a process still booting is not busy
    feed(proc, { type: "agent_start" });
    assert.equal(proc.isBusy(), true);
    feed(proc, { type: "agent_end", messages: [], willRetry: false });
    assert.equal(proc.isBusy(), false);
  });

  it("stays busy across agent_end with willRetry and clears at agent_settled", () => {
    const proc = offlineProcess();
    feed(proc, { type: "agent_start" });
    feed(proc, { type: "agent_end", messages: [], willRetry: true });
    assert.equal(proc.isBusy(), true);
    feed(proc, { type: "agent_settled" });
    assert.equal(proc.isBusy(), false);
  });
});

describe("PiProcess extension UI", () => {
  it("auto-cancels dialog requests and ignores other fire-and-forget methods", () => {
    const written = [];
    const proc = offlineProcess();
    proc.child = { stdin: { write: (s) => written.push(s) } };
    feed(proc, { type: "extension_ui_request", id: "dialog-1", method: "select", title: "Allow?", options: ["y", "n"] });
    feed(proc, { type: "extension_ui_request", id: "dialog-2", method: "confirm", title: "Sure?" });
    feed(proc, { type: "extension_ui_request", id: "ui-1", method: "setWidget", widgetKey: "k", widgetLines: ["x"] });
    feed(proc, { type: "extension_ui_request", id: "ui-2", method: "setStatus", statusKey: "k", statusText: "running" });
    assert.deepEqual(written, [
      JSON.stringify({ type: "extension_ui_response", id: "dialog-1", cancelled: true }) + "\n",
      JSON.stringify({ type: "extension_ui_response", id: "dialog-2", cancelled: true }) + "\n",
    ]);
  });

  it("captures the orchestrator extension's live widget and status", () => {
    const proc = offlineProcess();
    const lines = ["◉ orchestrator ⏳ running — Demo · 5s", "  ⏳ Step one", "  ✓ Step two"];
    feed(proc, {
      type: "extension_ui_request",
      id: "ui-1",
      method: "setWidget",
      widgetKey: "orchestrator",
      widgetLines: lines,
    });
    feed(proc, {
      type: "extension_ui_request",
      id: "ui-2",
      method: "setStatus",
      statusKey: "orchestrator",
      statusText: "orchestrator: running · 1/2 steps done · 1 worker running",
    });
    assert.deepEqual(proc.lastOrchestratorWidget, lines);
    assert.equal(proc.lastOrchestratorStatus, "orchestrator: running · 1/2 steps done · 1 worker running");

    // A clear (omitted lines/text) resets the capture; other keys never touch it.
    feed(proc, { type: "extension_ui_request", id: "ui-3", method: "setWidget", widgetKey: "orchestrator" });
    assert.equal(proc.lastOrchestratorWidget, null);
    feed(proc, { type: "extension_ui_request", id: "ui-4", method: "setWidget", widgetKey: "other", widgetLines: ["x"] });
    assert.equal(proc.lastOrchestratorWidget, null);
    feed(proc, { type: "extension_ui_request", id: "ui-5", method: "setStatus", statusKey: "orchestrator" });
    assert.equal(proc.lastOrchestratorStatus, null);
    assert.equal(proc.lastOrchestratorWidget, null);
  });
});

describe("PiProcess end-to-end against the fake", () => {
  let tmp;
  let sessionFile;
  let proc;

  before(async () => {
    tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-rpc-"));
    // a header-only session the fake will append to
    sessionFile = path.join(tmp, "e2e.jsonl");
    await fs.writeFile(
      sessionFile,
      JSON.stringify({ type: "session", version: 3, id: "e2e", timestamp: new Date().toISOString(), cwd: "/tmp" }) + "\n"
    );
    proc = new PiProcess({ binary: FAKE_PI, sessionFile, cwd: "/tmp", sessionDir: tmp });
    proc.start();
  });

  after(async () => {
    proc.kill();
    await fs.rm(tmp, { recursive: true, force: true });
  });

  it("correlates responses by id and streams a prompt to disk", async () => {
    const state = await proc.sendCommand("get_state");
    assert.equal(state.success, true);
    assert.equal(state.data.sessionFile, sessionFile);

    const accepted = await proc.sendCommand("prompt", { message: "hello fake pi" });
    assert.equal(accepted.success, true);

    // the overlay exposes the growing text while the run streams; agent_start
    // precedes message_start, so busy is guaranteed once the overlay appears
    let pending = null;
    for (let i = 0; i < 40 && !pending; i++) {
      pending = proc.getPendingMessage();
      if (!pending) await new Promise((r) => setTimeout(r, 25));
    }
    assert.ok(pending, "expected the in-flight overlay while streaming");
    assert.equal(proc.isBusy(), true);
    assert.ok(!pending.info.time.completed);

    // after agent_settled: not busy, no overlay, and the file has the entry
    for (let i = 0; i < 40 && proc.isBusy(); i++) await new Promise((r) => setTimeout(r, 25));
    assert.equal(proc.isBusy(), false);
    assert.equal(proc.getPendingMessage(), null);

    const content = await fs.readFile(sessionFile, "utf8");
    assert.match(content, /"role":"assistant"/);
    assert.match(content, /hello fake pi/);
  });
});

describe("PiPool", () => {
  it("registers processes by file and evicts the oldest idle one at the cap", async () => {
    const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-pool-"));
    const files = [];
    for (let i = 0; i < 3; i++) {
      const file = path.join(tmp, `s${i}.jsonl`);
      await fs.writeFile(file, JSON.stringify({ type: "session", version: 3, id: `s${i}`, timestamp: new Date().toISOString(), cwd: "/tmp" }) + "\n");
      files.push(file);
    }
    const pool = new PiPool({ binary: FAKE_PI, sessionDir: tmp, defaultCwd: "/tmp", maxProcesses: 2 });
    try {
      const p0 = await pool.ensure(files[0], "/tmp");
      await new Promise((r) => setTimeout(r, 50)); // let p0 age
      const p1 = await pool.ensure(files[1], "/tmp");
      await new Promise((r) => setTimeout(r, 50));
      const p2 = await pool.ensure(files[2], "/tmp"); // pushes p0 out
      assert.equal(pool.processes.size, 2);
      assert.equal(pool.getByFile(files[0]), null); // evicted
      assert.equal(pool.getByFile(files[1]), p1);
      assert.equal(pool.getByFile(files[2]), p2);
      for (let i = 0; i < 40 && !p0.exited; i++) await new Promise((r) => setTimeout(r, 10));
      assert.ok(p0.exited, "the evicted process was killed");
    } finally {
      pool.shutdown();
      await fs.rm(tmp, { recursive: true, force: true });
    }
  });

  it("resolves processes by session id for the newborn window", async () => {
    const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-pool-"));
    const file = path.join(tmp, "2026-08-10T21-00-00-000Z_019fed00-1234-7000-8000-000000000001.jsonl");
    await fs.writeFile(file, JSON.stringify({ type: "session", version: 3, id: "x", timestamp: new Date().toISOString(), cwd: "/tmp" }) + "\n");
    const pool = new PiPool({ binary: FAKE_PI, sessionDir: tmp, defaultCwd: "/tmp", maxProcesses: 2 });
    try {
      const proc = await pool.ensure(file, "/tmp");
      assert.equal(pool.getById("2026-08-10T21-00-00-000Z_019fed00-1234-7000-8000-000000000001"), proc);
      assert.equal(pool.getById("nope"), null);
    } finally {
      pool.shutdown();
      await fs.rm(tmp, { recursive: true, force: true });
    }
  });
});
