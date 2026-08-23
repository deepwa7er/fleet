// bridge/test/change-store.test.js
// The durable half of the change object, no jj involved: event-log append
// and replay, the transition table, additive rounds, and the crash
// tolerance the JSONL discipline promises (an unacknowledged half-written
// last line is skipped, an unknown event type from a newer bridge is
// skipped, neither is fatal).
import { before, after, describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createChangeStore } from "../lib/change-store.js";

let tmp;
let store;

before(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-change-store-"));
  store = createChangeStore({ dir: path.join(tmp, "changes") });
});

after(async () => {
  await fs.rm(tmp, { recursive: true, force: true });
});

const CHANGE_A = "k".repeat(32);
const CHANGE_B = "l".repeat(32);

describe("change store", () => {
  it("creates a change in working state and reads it back", async () => {
    const created = await store.create("demo", 1);
    assert.equal(created.state, "working");
    assert.deepEqual(created.rounds, []);
    const loaded = await store.get("demo", 1);
    assert.equal(loaded.repo, "demo");
    assert.equal(loaded.card, 1);
    assert.equal(loaded.state, "working");
  });

  it("rejects a second change for the same card", async () => {
    await assert.rejects(store.create("demo", 1), (err) => err.code === "EEXIST");
  });

  it("returns null for a change that does not exist", async () => {
    assert.equal(await store.get("demo", 999), null);
  });

  it("rejects path-shaped repo names and non-card numbers", async () => {
    await assert.rejects(store.create("../escape", 1), /invalid repo name/);
    await assert.rejects(store.create("demo", 0), /invalid card number/);
    await assert.rejects(store.create("demo", 1.5), /invalid card number/);
  });

  it("appends additive rounds and replays them in order", async () => {
    const r1 = await store.addRound("demo", 1, { author: "agent", changeId: CHANGE_A, note: "first pass" });
    assert.equal(r1.n, 1);
    const r2 = await store.addRound("demo", 1, { author: "you", changeId: CHANGE_B });
    assert.equal(r2.n, 2);
    assert.equal(r2.note, null);
    const change = await store.get("demo", 1);
    assert.deepEqual(
      change.rounds.map((r) => [r.n, r.author, r.changeId]),
      [
        [1, "agent", CHANGE_A],
        [2, "you", CHANGE_B],
      ]
    );
  });

  it("rejects a round reusing an existing change id", async () => {
    await assert.rejects(
      store.addRound("demo", 1, { author: "agent", changeId: CHANGE_A }),
      (err) => err.code === "DUPLICATE"
    );
  });

  it("rejects authors outside agent|you", async () => {
    await assert.rejects(
      Promise.resolve().then(() => store.addRound("demo", 1, { author: "robot", changeId: "m".repeat(32) })),
      /author must be one of/
    );
  });

  it("runs the caller's validation inside the append and aborts on rejection", async () => {
    await assert.rejects(
      store.addRound("demo", 1, { author: "agent", changeId: "m".repeat(32) }, async () => {
        throw new Error("repository said no");
      }),
      /repository said no/
    );
    const change = await store.get("demo", 1);
    assert.equal(change.rounds.length, 2, "a rejected round must not be appended");
  });

  it("attaches annotations to their round", async () => {
    const annotation = await store.addAnnotation("demo", 1, {
      id: "ann-1",
      round: 1,
      path: "a.txt",
      line: 3,
      side: "new",
      text: "cached because the phone re-polls",
    });
    assert.equal(annotation.round, 1);
    const change = await store.get("demo", 1);
    assert.equal(change.rounds[0].annotations.length, 1);
    assert.equal(change.rounds[0].annotations[0].text, "cached because the phone re-polls");
    assert.equal(change.rounds[1].annotations.length, 0);
  });

  it("rejects annotations for a round that does not exist", async () => {
    await assert.rejects(
      store.addAnnotation("demo", 1, { id: "ann-2", round: 9, path: "a.txt", line: 1, side: "new", text: "x" }),
      (err) => err.code === "NO_ROUND"
    );
  });

  it("walks the lifecycle and rejects illegal transitions", async () => {
    await assert.rejects(store.transition("demo", 1, "shipped"), (err) => err.code === "TRANSITION");
    assert.equal((await store.transition("demo", 1, "in_review")).state, "in_review");
    assert.equal((await store.transition("demo", 1, "working")).state, "working");
    assert.equal((await store.transition("demo", 1, "in_review")).state, "in_review");
    assert.equal((await store.transition("demo", 1, "landing")).state, "landing");
    assert.equal((await store.transition("demo", 1, "shipped")).state, "shipped");
    await assert.rejects(store.transition("demo", 1, "working"), (err) => err.code === "TRANSITION");
  });

  it("freezes rounds and annotations once the change leaves review", async () => {
    await assert.rejects(
      store.addRound("demo", 1, { author: "agent", changeId: "n".repeat(32) }),
      (err) => err.code === "FROZEN"
    );
    await assert.rejects(
      store.addAnnotation("demo", 1, { id: "ann-3", round: 1, path: "a.txt", line: 1, side: "new", text: "x" }),
      (err) => err.code === "FROZEN"
    );
  });

  it("refuses to submit a change with no rounds", async () => {
    await store.create("demo", 2);
    await assert.rejects(store.transition("demo", 2, "in_review"), /no rounds; nothing to review/);
  });

  it("skips a half-written last line instead of failing the change", async () => {
    const file = path.join(tmp, "changes", "demo", "2.jsonl");
    await fs.appendFile(file, '{"event":"round","n":1,"author":"agent","chan');
    const change = await store.get("demo", 2);
    assert.equal(change.state, "working");
    assert.equal(change.rounds.length, 0);
  });

  it("skips event types it does not know", async () => {
    const file = path.join(tmp, "changes", "demo", "2.jsonl");
    await fs.appendFile(file, "\n" + JSON.stringify({ event: "from-the-future", at: "2036-01-01T00:00:00Z" }) + "\n");
    const change = await store.get("demo", 2);
    assert.equal(change.state, "working");
  });

  it("keeps title, session, and per-round claims through replay", async () => {
    await store.create("demo", 3, { title: "pi model picker", session: "pi:abc" });
    await store.addRound("demo", 3, {
      author: "agent",
      changeId: "p".repeat(32),
      gatesRan: ["cargo test", "clippy"],
      worthKnowing: ["+1 dependency (serde_yaml)"],
    });
    const change = await store.get("demo", 3);
    assert.equal(change.title, "pi model picker");
    assert.equal(change.session, "pi:abc");
    assert.deepEqual(change.rounds[0].gatesRan, ["cargo test", "clippy"]);
    assert.deepEqual(change.rounds[0].worthKnowing, ["+1 dependency (serde_yaml)"]);
    const rebound = await store.setSession("demo", 3, "muse:def");
    assert.equal(rebound.session, "muse:def");
    assert.equal((await store.get("demo", 3)).session, "muse:def");
  });

  it("rejects malformed claims on a round", async () => {
    await assert.rejects(
      store.addRound("demo", 3, { author: "agent", changeId: "q".repeat(32), gatesRan: ["ok", ""] }),
      /gatesRan must be an array of non-empty strings/
    );
  });

  it("records a request atomically with the reopen", async () => {
    await assert.rejects(store.requestChanges("demo", 3, "not yet in review"), (err) => err.code === "TRANSITION");
    await store.transition("demo", 3, "in_review");
    const change = await store.requestChanges("demo", 3, "cache the model list");
    assert.equal(change.state, "working");
    assert.equal(change.lastRequest.note, "cache the model list");
    assert.equal((await store.get("demo", 3)).lastRequest.note, "cache the model list");
  });

  it("records landing outcomes and their states", async () => {
    await store.transition("demo", 3, "in_review");
    await assert.rejects(store.completeLanding("demo", 3, { tip: "abc" }), (err) => err.code === "TRANSITION");
    await store.transition("demo", 3, "landing");
    const failed = await store.failLanding("demo", 3, { reason: "the rebase conflicts", conflicts: ["r".repeat(32)] });
    assert.equal(failed.state, "in_review");
    let change = await store.get("demo", 3);
    assert.equal(change.lastLanding.ok, false);
    assert.deepEqual(change.lastLanding.conflicts, ["r".repeat(32)]);
    await store.transition("demo", 3, "landing");
    const shipped = await store.completeLanding("demo", 3, { tip: "0123abcd" });
    assert.equal(shipped.state, "shipped");
    await store.recordCardComment("demo", 3, { ok: false, message: "fizzy was down" });
    change = await store.get("demo", 3);
    assert.equal(change.landed.tip, "0123abcd");
    assert.equal(change.lastLanding.ok, true);
    assert.equal(change.cardComment.ok, false);
    assert.match(change.cardComment.message, /fizzy was down/);
  });

  it("lists every change, newest activity first", async () => {
    const changes = await store.list();
    assert.deepEqual(
      changes.map((c) => [c.repo, c.card]).sort((a, b) => a[1] - b[1]),
      [
        ["demo", 1],
        ["demo", 2],
        ["demo", 3],
      ]
    );
    // demo/3's landing walk is the most recent activity in this suite.
    assert.equal(changes[0].card, 3);
  });
});
