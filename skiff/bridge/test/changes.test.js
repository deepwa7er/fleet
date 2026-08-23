// bridge/test/changes.test.js
// The change object end to end: a real bridge on an ephemeral port, a real
// temp jj repository for the rounds, and the /change HTTP family driven the
// way skiff (and the step-03 review) will drive it. Requires the jj binary;
// on a host without one the suite skips visibly rather than faking the
// repository — the jj semantics (change ids, parentage, diffs) are the
// thing under test.
import { before, after, describe, it } from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createBridge } from "../server.js";
import { diffFilePaths, isFullChangeId } from "../lib/jj.js";
import { resolveBinary } from "../lib/resolve-binary.js";

const execFileAsync = promisify(execFile);

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES = path.join(HERE, "fixtures");
const PASSWORD = "test-password";

function tryResolveJj() {
  try {
    return resolveBinary("jj", process.env.JJ_BINARY);
  } catch {
    return null;
  }
}
const JJ = tryResolveJj();

// The pure helpers need no repository.
describe("jj helpers", () => {
  it("accepts only full 32-character change ids", () => {
    assert.equal(isFullChangeId("k".repeat(32)), true);
    assert.equal(isFullChangeId("k".repeat(31)), false);
    assert.equal(isFullChangeId("a".repeat(32)), false, "hex digits are not in the change id alphabet");
    assert.equal(isFullChangeId("all()"), false);
    assert.equal(isFullChangeId(42), false);
  });

  it("collects both sides of every file a git diff touches", () => {
    const diff = [
      "diff --git a/old name.txt b/new name.txt",
      "rename from old name.txt",
      "rename to new name.txt",
      "diff --git a/skiff/app.rb b/skiff/app.rb",
      "index 000..111 100644",
      "+++ b/skiff/app.rb",
    ].join("\n");
    const paths = diffFilePaths(diff);
    assert.deepEqual([...paths].sort(), ["new name.txt", "old name.txt", "skiff/app.rb"]);
  });
});

describe("the /change family", { skip: JJ ? false : "jj binary not found (set JJ_BINARY)" }, () => {
  let tmp;
  let bridge;
  let base;
  // Change ids for the fixture stack: r1 ← r2 ← r3, plus a sibling child of
  // r1 that no additive round sequence can legally contain.
  let r1, r2, r3, sibling;

  async function jj(args, cwd) {
    const { stdout } = await execFileAsync(JJ, args, { cwd });
    return stdout;
  }

  async function changeIdAt(repoDir) {
    return (await jj(["log", "--no-graph", "-r", "@", "-T", "change_id"], repoDir)).trim();
  }

  before(async () => {
    tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-changes-"));
    // A hermetic jj config: the suite must not depend on (or disturb) the
    // developer's identity, and the bridge's read-only spawns inherit it.
    const jjConfig = path.join(tmp, "jj-config.toml");
    await fs.writeFile(jjConfig, '[user]\nname = "Test"\nemail = "test@example.invalid"\n');
    process.env.JJ_CONFIG = jjConfig;

    const reposDir = path.join(tmp, "repos");
    const repoDir = path.join(reposDir, "demo");
    await fs.mkdir(repoDir, { recursive: true });
    await jj(["git", "init", "--colocate"], repoDir);

    await fs.writeFile(path.join(repoDir, "a.txt"), "one\n");
    await jj(["describe", "-m", "round 1: implementation"], repoDir);
    r1 = await changeIdAt(repoDir);

    await jj(["new"], repoDir);
    await fs.writeFile(path.join(repoDir, "a.txt"), "one\ntwo\n");
    await jj(["describe", "-m", "round 2: revision"], repoDir);
    r2 = await changeIdAt(repoDir);

    await jj(["new"], repoDir);
    await fs.writeFile(path.join(repoDir, "b.txt"), "three\n");
    await jj(["describe", "-m", "round 3: while in review"], repoDir);
    r3 = await changeIdAt(repoDir);

    await jj(["new", r1], repoDir);
    await fs.writeFile(path.join(repoDir, "c.txt"), "stray\n");
    await jj(["describe", "-m", "sibling of round 2"], repoDir);
    sibling = await changeIdAt(repoDir);

    bridge = createBridge({
      password: PASSWORD,
      host: "127.0.0.1",
      port: 0,
      defaultCwd: tmp,
      pi: { sessionDir: tmp, binary: path.join(FIXTURES, "fake-pi.mjs") },
      muse: { sessionDir: path.join(tmp, "muse", "sessions"), binary: path.join(FIXTURES, "fake-muse.mjs") },
      opencode: { url: "http://127.0.0.1:1" },
      changes: { dir: path.join(tmp, "changes"), reposDir, binary: JJ },
    });
    await bridge.listen();
    base = `http://127.0.0.1:${bridge.port()}`;
  });

  after(async () => {
    await bridge?.close();
    delete process.env.JJ_CONFIG;
    await fs.rm(tmp, { recursive: true, force: true });
  });

  const AUTH = "Basic " + Buffer.from(`skiff:${PASSWORD}`).toString("base64");
  const get = (p) => fetch(base + p, { headers: { Authorization: AUTH } });
  const post = (p, body) =>
    fetch(base + p, {
      method: "POST",
      headers: { Authorization: AUTH, "Content-Type": "application/json" },
      body: JSON.stringify(body ?? {}),
    });

  it("creates a change bound to a card", async () => {
    const response = await post("/change", { repo: "demo", card: 81 });
    assert.equal(response.status, 201);
    const change = await response.json();
    assert.equal(change.card, 81);
    assert.equal(change.state, "working");
  });

  it("refuses a second change for the same card, unknown repos, and bad cards", async () => {
    assert.equal((await post("/change", { repo: "demo", card: 81 })).status, 409);
    assert.equal((await post("/change", { repo: "nowhere", card: 81 })).status, 404);
    assert.equal((await post("/change", { repo: "demo", card: "eighty-one" })).status, 400);
    assert.equal((await post("/change", { repo: "../demo", card: 81 })).status, 400);
  });

  it("appends round 1 from its jj change id", async () => {
    const response = await post("/change/demo/81/round", { author: "agent", changeId: r1, note: "asked for a model picker" });
    assert.equal(response.status, 201);
    const round = await response.json();
    assert.equal(round.n, 1);
    assert.equal(round.changeId, r1);
  });

  it("rejects rounds that are not additive", async () => {
    // A change id that resolves to nothing. (Not all-z: that is the virtual
    // root commit's change id, which does resolve.)
    const missing = await post("/change/demo/81/round", { author: "agent", changeId: "m".repeat(32) });
    assert.equal(missing.status, 400);
    assert.match((await missing.json()).error, /does not exist/);
    // A commit that is not a child of round 1's tip.
    const stray = await post("/change/demo/81/round", { author: "agent", changeId: r3 });
    assert.equal(stray.status, 400);
    assert.match((await stray.json()).error, /must be a child of round 1/);
    // Not a change id at all.
    assert.equal((await post("/change/demo/81/round", { author: "agent", changeId: "@" })).status, 400);
    // Not an author the model knows.
    assert.equal((await post("/change/demo/81/round", { author: "robot", changeId: r2 })).status, 400);
  });

  it("appends round 2 as a child of round 1, and refuses the sibling", async () => {
    assert.equal((await post("/change/demo/81/round", { author: "agent", changeId: r2 })).status, 201);
    const rejected = await post("/change/demo/81/round", { author: "agent", changeId: sibling });
    assert.equal(rejected.status, 400);
    assert.match((await rejected.json()).error, /must be a child of round 2/);
    const duplicate = await post("/change/demo/81/round", { author: "agent", changeId: r2 });
    assert.equal(duplicate.status, 409);
  });

  it("serves the change with rounds enriched from the repository", async () => {
    const response = await get("/change/demo/81");
    assert.equal(response.status, 200);
    const change = await response.json();
    assert.equal(change.path, path.join(tmp, "repos", "demo"), "the one place a filesystem path surfaces");
    assert.equal(change.rounds.length, 2);
    assert.equal(change.rounds[0].commit.description, "round 1: implementation");
    assert.equal(change.rounds[1].commit.description, "round 2: revision");
    assert.ok(change.rounds[1].commit.parents.includes(r1));
  });

  it("serves this round's diff and the cumulative diff", async () => {
    const round2 = await (await get("/change/demo/81/diff/2")).json();
    assert.match(round2.diff, /\+two/);
    assert.doesNotMatch(round2.diff, /\+one/);
    const cumulative = await (await get("/change/demo/81/diff")).json();
    assert.match(cumulative.diff, /\+one/);
    assert.match(cumulative.diff, /\+two/);
    assert.equal((await get("/change/demo/81/diff/9")).status, 404);
  });

  it("positions annotations in a round's diff and rejects files the round never touched", async () => {
    const good = await post("/change/demo/81/annotation", {
      round: 2,
      path: "a.txt",
      line: 2,
      text: "grew the list instead of replacing it",
    });
    assert.equal(good.status, 201);
    const annotation = await good.json();
    assert.equal(annotation.side, "new", "side defaults to the new side of the diff");
    const untouched = await post("/change/demo/81/annotation", { round: 2, path: "b.txt", line: 1, text: "x" });
    assert.equal(untouched.status, 400);
    assert.match((await untouched.json()).error, /does not touch b\.txt/);
    assert.equal((await post("/change/demo/81/annotation", { round: 9, path: "a.txt", line: 1, text: "x" })).status, 404);
    assert.equal((await post("/change/demo/81/annotation", { round: 2, path: "a.txt", line: 0, text: "x" })).status, 400);
    const enriched = await (await get("/change/demo/81")).json();
    assert.equal(enriched.rounds[1].annotations.length, 1);
  });

  it("submits for review, keeps rounds open during review, and reopens", async () => {
    assert.equal((await (await post("/change/demo/81/submit")).json()).state, "in_review");
    assert.equal((await post("/change/demo/81/submit")).status, 409);
    // "Edit it yourself" lands as a round authored by you, mid-review.
    const yours = await post("/change/demo/81/round", { author: "you", changeId: r3 });
    assert.equal(yours.status, 201);
    assert.equal((await yours.json()).author, "you");
    assert.equal((await (await post("/change/demo/81/reopen")).json()).state, "working");
    assert.equal((await post("/change/demo/81/reopen")).status, 409);
  });

  it("lists changes and survives a bridge restart from the log alone", async () => {
    const listed = await (await get("/change")).json();
    assert.equal(listed.changes.length, 1);
    assert.equal(listed.changes[0].card, 81);

    const reopened = createBridge({
      password: PASSWORD,
      host: "127.0.0.1",
      port: 0,
      defaultCwd: tmp,
      pi: { sessionDir: tmp, binary: path.join(FIXTURES, "fake-pi.mjs") },
      muse: { sessionDir: path.join(tmp, "muse", "sessions"), binary: path.join(FIXTURES, "fake-muse.mjs") },
      opencode: { url: "http://127.0.0.1:1" },
      changes: { dir: path.join(tmp, "changes"), reposDir: path.join(tmp, "repos"), binary: JJ },
    });
    await reopened.listen();
    try {
      const again = await fetch(`http://127.0.0.1:${reopened.port()}/change/demo/81`, {
        headers: { Authorization: AUTH },
      });
      const change = await again.json();
      assert.equal(change.state, "working");
      assert.equal(change.rounds.length, 3);
      assert.equal(change.rounds[1].annotations.length, 1);
    } finally {
      await reopened.close();
    }
  });

  it("session payloads carry the ref of the change bound to them", async () => {
    // Newborn sessions from the fake pi harness get wire ids to bind to.
    const created = await post("/session", { harness: "pi", title: "bound session" });
    assert.equal(created.status, 201);
    const { id } = await created.json();
    assert.match(id, /^pi:/);
    const other = await (await post("/session", { harness: "pi", title: "plain session" })).json();

    assert.equal((await post("/change", { repo: "demo", card: 82, session: id })).status, 201);
    assert.equal((await post("/change", { repo: "demo", card: 83 })).status, 201);

    const list = await (await get("/session")).json();
    const bound = list.sessions.find((s) => s.id === id);
    assert.ok(bound, "the newborn session is listed");
    // The bound change is the ref the session page renders (DW-002 §6):
    // repo/card to fetch it, and the facts the list items show.
    assert.equal(bound.change.repo, "demo");
    assert.equal(bound.change.card, 82);
    assert.equal(bound.change.state, "working");
    assert.equal(bound.change.rounds, 0);
    assert.equal(bound.change.title, null);
    assert.ok(bound.change.updatedAt, "the ref carries an updatedAt");

    // The show endpoint carries the same ref.
    const show = await (await get(`/session/${id}`)).json();
    assert.equal(show.change.repo, "demo");
    assert.equal(show.change.card, 82);

    // A session with no bound change carries no ref at all.
    const plain = list.sessions.find((s) => s.id === other.id);
    assert.ok(plain);
    assert.equal(plain.change, undefined);

    // Rebinding a session moves the ref (a card can outlive the session
    // that started it, and the session can move to a newer card).
    assert.equal((await post("/change/demo/83/session", { session: id })).status, 200);
    const rebound = await (await get("/session")).json();
    const moved = rebound.sessions.find((s) => s.id === id);
    assert.equal(moved.change.card, 83);
  });
});
