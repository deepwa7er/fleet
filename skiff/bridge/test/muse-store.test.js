// bridge/test/muse-store.test.js
// Pure-function tests for the muse session store: the dated-layout walk (and
// its subagent exclusion), the session object, and the event-log -> {info,
// parts} transcript mapping, driven by the static fixture under
// fixtures/muse-sessions/ — a hand-written session log with metadata, an
// automatic name, one run (tool batch + result + text), encrypted reasoning,
// task noise, and a corrupt trailing line.
import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  listMuseSessionFiles,
  resolveMuseSessionFile,
  readMuseSessionFile,
  buildMuseSessionObject,
  mapMuseMessages,
  listMuseSessions,
} from "../lib/muse-store.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SESSIONS = path.join(HERE, "fixtures", "muse-sessions");
const ID = "26ea1b5e-0000-4000-8000-0000000000f1";

let parsed;

before(async () => {
  const file = await resolveMuseSessionFile(SESSIONS, ID);
  parsed = await readMuseSessionFile(file);
});

describe("muse session layout", () => {
  it("lists exactly the dated top-level sessions, never subagent children", async () => {
    const files = await listMuseSessionFiles(SESSIONS);
    assert.equal(files.length, 1);
    assert.ok(files[0].endsWith(path.join(ID, "session.jsonl")));
  });

  it("resolves a session id to its file and unknown ids to null", async () => {
    assert.ok(await resolveMuseSessionFile(SESSIONS, ID));
    assert.equal(await resolveMuseSessionFile(SESSIONS, "9dead000-0000-4000-8000-000000000001"), null);
  });
});

describe("muse session object", () => {
  it("carries the automatic name, workspace, model, and times", async () => {
    const session = buildMuseSessionObject(ID, parsed);
    assert.equal(session.id, ID);
    assert.equal(session.title, "lemon-aurora");
    assert.equal(session.directory, "/home/deepwater/code/skiff");
    assert.deepEqual(session.model, { id: "muse-spark-1.2" });
    // recorded_at is µs; the session object is ms.
    assert.equal(session.time.created, 1786395600000);
    assert.equal(session.time.updated, 1786395613000);
  });

  it("lists sessions with the same object shape", async () => {
    const sessions = await listMuseSessions(SESSIONS);
    assert.equal(sessions.length, 1);
    assert.equal(sessions[0].title, "lemon-aurora");
  });
});

describe("muse transcript mapping", () => {
  it("maps runs to user/assistant messages and folds tool results", () => {
    const messages = mapMuseMessages(parsed.records);
    assert.equal(messages.length, 3);

    assert.equal(messages[0].info.role, "user");
    assert.equal(messages[0].parts[0].text, "Explain the muse store design");

    // The tool batch: a tool part, folded to completed by the result batch.
    assert.equal(messages[1].info.role, "assistant");
    assert.equal(messages[1].info.agent, "muse-spark-1.2");
    assert.equal(messages[1].parts.length, 1);
    assert.deepEqual(messages[1].parts[0], {
      type: "tool",
      tool: "bash",
      id: "call_fixture_01",
      state: { status: "completed", output: "errors.js\nfile-tail.js\nids.js" },
    });

    // The committed text, with completed set so skiff's working fallback
    // never fires for a settled run.
    assert.equal(messages[2].info.role, "assistant");
    assert.equal(messages[2].parts[0].text, "The muse store maps the event log to a transcript.");
    assert.equal(messages[2].info.time.completed, messages[2].info.time.created);
  });

  it("falls back to the first prompt as the title when no name record exists", () => {
    // TUI sessions carry no session.name.changed; the first prompt titles
    // them, like muse's own session index.
    const withoutName = parsed.records.filter((r) => r.payload_type !== "session.name.changed");
    const session = buildMuseSessionObject(ID, { records: withoutName });
    assert.equal(session.title, "Explain the muse store design");
  });

  it("renders nothing for encrypted reasoning, task noise, and terminals", () => {
    const messages = mapMuseMessages(parsed.records);
    const texts = messages.flatMap((m) => m.parts).map((p) => p.text ?? "");
    assert.ok(!texts.some((t) => t.includes("encrypted")), "encrypted reasoning must never render");
  });
});
