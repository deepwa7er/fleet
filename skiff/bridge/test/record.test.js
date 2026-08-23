// bridge/test/record.test.js
// buildEntry is the privacy boundary (DW-003 §3) — these tests pin what
// crosses it and, more importantly, what never does. Exclusion is the
// default: the fixture deliberately carries every private field the change
// object has, and the entry must contain none of them.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { buildEntry } from "../lib/record.js";

const CHANGE = {
  repo: "fleet",
  card: 81,
  title: "pi model picker",
  session: "pi:ses_secret",
  path: "/home/deepwater/code/fleet",
  state: "shipped",
  createdAt: "2026-08-23T10:00:00Z",
  updatedAt: "2026-08-23T12:00:00Z",
  lastRequest: { note: "private review conversation", at: "2026-08-23T11:00:00Z" },
  landed: { tip: "57e14554a0bb", at: "2026-08-23T12:00:00Z" },
  lastLanding: { ok: true, at: "2026-08-23T12:00:00Z" },
  cardComment: { ok: true, at: "2026-08-23T12:00:01Z" },
  rounds: [
    {
      n: 1,
      author: "agent",
      changeId: "k".repeat(32),
      note: "what prompted it — private",
      gatesRan: [ "cargo test" ],
      worthKnowing: [ "+1 dependency" ],
      createdAt: "2026-08-23T10:30:00Z",
      commit: { commitId: "abc123", description: "round 1", authorEmail: "x@y", timestamp: "…", parents: [] },
      annotations: [ { id: "uuid-internal", path: "a.rb", line: 3, side: "new", text: "cached because…" } ],
    },
  ],
};

describe("buildEntry", () => {
  it("carries the public subset", () => {
    const entry = buildEntry(CHANGE, new Map([ [ 1, "diff --git …" ] ]));
    assert.equal(entry.repo, "fleet");
    assert.equal(entry.card, 81);
    assert.equal(entry.title, "pi model picker");
    assert.equal(entry.landedAt, "2026-08-23T12:00:00Z");
    assert.equal(entry.tip, "57e14554a0bb");
    assert.deepEqual(entry.afterward, []);
    const round = entry.rounds[0];
    assert.equal(round.author, "agent");
    assert.equal(round.commit, "abc123");
    assert.equal(round.diff, "diff --git …");
    assert.deepEqual(round.gatesRan, [ "cargo test" ]);
    assert.deepEqual(round.annotations, [ { path: "a.rb", line: 3, side: "new", text: "cached because…" } ]);
  });

  it("exports none of the private fields, and unknown fields do not leak", () => {
    const withFuture = { ...CHANGE, futureField: "added later, private until exported deliberately" };
    const entry = buildEntry(withFuture, new Map());
    const raw = JSON.stringify(entry);
    assert.ok(!raw.includes("pi:ses_secret"), "session ids never leave");
    assert.ok(!raw.includes("/home/deepwater"), "filesystem paths never leave");
    assert.ok(!raw.includes("what prompted it"), "round notes never leave");
    assert.ok(!raw.includes("private review conversation"), "request notes never leave");
    assert.ok(!raw.includes("uuid-internal"), "internal ids never leave");
    assert.ok(!raw.includes("futureField"), "exclusion is the default for new fields");
    assert.equal(entry.rounds[0].diff, null, "a missing diff is null, never a guess");
  });
});
