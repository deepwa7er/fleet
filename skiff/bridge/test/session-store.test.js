// bridge/test/session-store.test.js
// Unit tests for the pure file reader/mapper against the fixture files.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  readSessionFromFile,
  readMessagesFromFile,
  listSessions,
  resolveSessionFile,
  defaultSessionDir,
} from "../lib/session-store.js";

const FIXTURES = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures");
const fixture = (name) => path.join(FIXTURES, name);

describe("session-store", () => {
  describe("session objects", () => {
    it("builds the full shape from a multi-turn session", async () => {
      const session = await readSessionFromFile(fixture("multi-turn.jsonl"));
      assert.equal(session.id, "multi-turn");
      assert.equal(session.title, "Multi-turn fixture");
      assert.equal(session.directory, "/home/deepwater/code/skiff");
      assert.equal(session.time.created, Date.parse("2026-08-10T21:00:00.000Z"));
      assert.equal(session.time.updated, Date.parse("2026-08-10T21:00:22.000Z"));
      // the model_change entry on the leaf path wins over the assistant model
      assert.deepEqual(session.model, { id: "deepseek-v4-flash" });
      // no orchestrator-mode entry in this fixture, so the mode is off
      assert.deepEqual(session.orchestrator, { active: false });
    });

    it("surfaces the orchestrator mode from the last recorded toggle", async () => {
      const os = await import("node:os");
      const fs = await import("node:fs/promises");
      const dir = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-store-"));
      const file = path.join(dir, "orchestrated.jsonl");
      await fs.writeFile(
        file,
        [
          JSON.stringify({ type: "session", version: 3, id: "s1", timestamp: "2026-08-10T22:00:00.000Z", cwd: "/tmp" }),
          JSON.stringify({ type: "message", id: "u1", parentId: null, timestamp: "2026-08-10T22:00:01.000Z", message: { role: "user", content: "hi" } }),
          JSON.stringify({ type: "custom", id: "o1", parentId: "u1", timestamp: "2026-08-10T22:00:02.000Z", customType: "orchestrator-mode", data: { active: true, at: 1 } }),
          // other extensions' custom entries must not count
          JSON.stringify({ type: "custom", id: "x1", parentId: "o1", timestamp: "2026-08-10T22:00:03.000Z", customType: "other-extension", data: { active: true } }),
          JSON.stringify({ type: "custom", id: "o2", parentId: "x1", timestamp: "2026-08-10T22:00:04.000Z", customType: "orchestrator-mode", data: { active: false, at: 2 } }),
        ].join("\n") + "\n"
      );

      const session = await readSessionFromFile(file);
      // the last orchestrator-mode entry wins, not the first
      assert.deepEqual(session.orchestrator, { active: false });

      await fs.rm(dir, { recursive: true, force: true });
    });

    it("treats an entry without a boolean active as mode off", async () => {
      const os = await import("node:os");
      const fs = await import("node:fs/promises");
      const dir = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-store-"));
      const file = path.join(dir, "malformed.jsonl");
      await fs.writeFile(
        file,
        [
          JSON.stringify({ type: "session", version: 3, id: "s1", timestamp: "2026-08-10T22:00:00.000Z", cwd: "/tmp" }),
          JSON.stringify({ type: "custom", id: "o1", parentId: null, timestamp: "2026-08-10T22:00:02.000Z", customType: "orchestrator-mode", data: { at: 1 } }),
        ].join("\n") + "\n"
      );

      const session = await readSessionFromFile(file);
      assert.deepEqual(session.orchestrator, { active: false });

      await fs.rm(dir, { recursive: true, force: true });
    });

    it("falls back to the last assistant message's model without a model_change", async () => {
      const session = await readSessionFromFile(fixture("trailing-partial.jsonl"));
      assert.deepEqual(session.model, { id: "deepseek-v4-flash" });
    });

    it("treats a header-only session as a session with null title and model", async () => {
      const session = await readSessionFromFile(fixture("empty.jsonl"));
      assert.equal(session.id, "empty");
      assert.equal(session.title, null);
      assert.equal(session.model, null);
      assert.equal(session.time.created, session.time.updated);
    });

    it("lists every fixture, most recently active first", async () => {
      const sessions = await listSessions(FIXTURES);
      const ids = sessions.map((s) => s.id);
      assert.deepEqual([...ids].sort(), ["branched", "empty", "multi-turn", "trailing-partial"]);
      // updated desc: empty (21:30Z header) > trailing-partial (21:20:08Z) > branched (21:10:45Z) > multi-turn (21:00:22Z)
      assert.deepEqual(ids, ["empty", "trailing-partial", "branched", "multi-turn"]);
    });
  });

  describe("session id resolution", () => {
    it("resolves an id to its file by basename", async () => {
      const file = await resolveSessionFile(FIXTURES, "multi-turn");
      assert.equal(file, fixture("multi-turn.jsonl"));
    });

    it("returns null for an unknown id", async () => {
      assert.equal(await resolveSessionFile(FIXTURES, "does-not-exist"), null);
    });

    it("defaults the session dir from env or the pi default", () => {
      const original = process.env.PI_SESSION_DIR;
      try {
        delete process.env.PI_SESSION_DIR;
        assert.equal(defaultSessionDir(), path.join(process.env.HOME, ".pi", "agent", "sessions"));
        process.env.PI_SESSION_DIR = "/tmp/custom-sessions";
        assert.equal(defaultSessionDir(), "/tmp/custom-sessions");
      } finally {
        if (original === undefined) delete process.env.PI_SESSION_DIR;
        else process.env.PI_SESSION_DIR = original;
      }
    });
  });

  describe("message mapping", () => {
    it("maps user and assistant messages with reasoning, tool pairing, and file parts", async () => {
      const messages = await readMessagesFromFile(fixture("multi-turn.jsonl"));
      // leaf path: user(a) -> assistant(b) -> toolResult(c, folded into b)
      //         -> assistant(d) -> user(e) -> assistant(f): 5 rendered messages
      assert.equal(messages.length, 5);

      const [user0, assistant0] = messages;
      assert.equal(user0.info.id, "aaaaaaaa");
      assert.equal(user0.info.role, "user");
      assert.deepEqual(user0.info.time, { created: Date.parse("2026-08-10T21:00:05.000Z") });
      assert.equal(user0.parts.length, 2);
      assert.deepEqual(user0.parts[0], {
        type: "text",
        text: "Explain the bridge design and attach a diagram.",
        id: "aaaaaaaa-p0",
      });
      assert.deepEqual(user0.parts[1], { type: "file", filename: "image.png", id: "aaaaaaaa-p1" });

      assert.equal(assistant0.info.role, "assistant");
      assert.equal(assistant0.info.agent, "deepseek-v4-flash");
      assert.equal(assistant0.info.time.completed, assistant0.info.time.created);
      assert.deepEqual(assistant0.parts.map((p) => p.type), ["reasoning", "text", "tool"]);
      assert.equal(assistant0.parts[0].type, "reasoning");
      assert.match(assistant0.parts[0].text, /design explanation/);
      assert.equal(assistant0.parts[0].id, "bbbbbbbb-p0");
      assert.match(assistant0.parts[1].text, /bridge design/);
      assert.equal(assistant0.parts[1].id, "bbbbbbbb-p1");
      assert.equal(assistant0.parts[2].type, "tool");
      assert.equal(assistant0.parts[2].tool, "bash");
      assert.equal(assistant0.parts[2].id, "call_00_probe");
      assert.equal(assistant0.parts[2].state.status, "completed");
      assert.match(assistant0.parts[2].state.output, /pi-rpc\.js/);
    });

    it("folds the toolResult into the tool part and keeps message order", async () => {
      const messages = await readMessagesFromFile(fixture("multi-turn.jsonl"));
      const toolPart = messages[1].parts[2];
      assert.equal(toolPart.state.status, "completed");
      assert.match(toolPart.state.output, /session-store\.js/);
      // the toolResult entry itself produces no message
      assert.equal(messages.length, 5);
    });

    it("walks only the leaf branch: abandoned branches never surface", async () => {
      const messages = await readMessagesFromFile(fixture("branched.jsonl"));
      const texts = messages
        .flatMap((m) => m.parts.filter((p) => p.type === "text").map((p) => p.text));
      assert.deepEqual(texts, [
        "first question",
        "first answer",
        "branch point question",
        "new question on branch B (current)",
        "answer on branch B (current)",
      ]);
    });

    it("tolerates a trailing partial line (mid-write file)", async () => {
      const messages = await readMessagesFromFile(fixture("trailing-partial.jsonl"));
      const texts = messages.flatMap((m) => m.parts.map((p) => p.text).filter(Boolean));
      assert.deepEqual(texts, ["hello before the crash", "a complete reply"]);
    });

    it("returns an empty message list for a header-only session", async () => {
      const messages = await readMessagesFromFile(fixture("empty.jsonl"));
      assert.deepEqual(messages, []);
    });
  });

  describe("synthetic entries", () => {
    // A compaction entry mid-branch renders as a text part; the surrounding
    // messages still map normally.
    it("renders compaction entries as text parts", async () => {
      const { readMessagesFromFile: read, readSessionFile } = await import("../lib/session-store.js");
      // build a session in-memory through the same entry pipeline by writing
      // a temp file — the store is file-based by design.
      const os = await import("node:os");
      const fs = await import("node:fs/promises");
      const dir = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-store-"));
      const file = path.join(dir, "compacted.jsonl");
      await fs.writeFile(
        file,
        [
          JSON.stringify({ type: "session", version: 3, id: "s1", timestamp: "2026-08-10T22:00:00.000Z", cwd: "/tmp" }),
          JSON.stringify({ type: "message", id: "u1", parentId: null, timestamp: "2026-08-10T22:00:01.000Z", message: { role: "user", content: "early" } }),
          JSON.stringify({ type: "compaction", id: "c1", parentId: "u1", timestamp: "2026-08-10T22:00:02.000Z", summary: "User discussed X and Y." }),
          JSON.stringify({ type: "message", id: "u2", parentId: "c1", timestamp: "2026-08-10T22:00:03.000Z", message: { role: "user", content: "later" } }),
        ].join("\n") + "\n"
      );
      const messages = await read(file);
      assert.equal(messages.length, 3);
      assert.equal(messages[1].info.role, "assistant");
      assert.equal(messages[1].parts[0].type, "text");
      assert.equal(messages[1].parts[0].text, "User discussed X and Y.");
      // synthetic entries carry time.completed so skiff's working fallback
      // never mistakes a finished summary for a live stream
      assert.ok(messages[1].info.time.completed);
      const session = await readSessionFromFile(file);
      assert.equal(session.title, null);
      await fs.rm(dir, { recursive: true, force: true });
    });
  });
});
