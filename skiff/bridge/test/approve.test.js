// bridge/test/approve.test.js
// The three verbs end to end (DW-002 §5–6): a real bridge, a real temp jj
// repository with a real bare git origin, a stub Fizzy taking the card
// comment, and the fake pi harness receiving request-changes notes. Covers
// approve's happy path, the conflict landing (and the resolve-and-reland
// cycle after it), the exhausted push race via a rejecting pre-receive
// hook, and the request-changes loop. Requires jj and git; skips visibly
// without them.
import { before, after, describe, it } from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createBridge } from "../server.js";
import { resolveBinary } from "../lib/resolve-binary.js";

const execFileAsync = promisify(execFile);

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES = path.join(HERE, "fixtures");
const PASSWORD = "test-password";

function tryResolve(name) {
  try {
    return resolveBinary(name, process.env[`${name.toUpperCase()}_BINARY`]);
  } catch {
    return null;
  }
}
const JJ = tryResolve("jj");
const GIT = tryResolve("git");
const SKIP = JJ && GIT ? false : "jj and git binaries required (set JJ_BINARY / GIT_BINARY)";

describe("the three verbs", { skip: SKIP }, () => {
  let tmp;
  let repoDir;
  let originDir;
  let recordDir;
  let recordOriginDir;
  let bridge;
  let base;
  let fizzyStub;
  let fizzyRequests;

  async function jj(args) {
    const { stdout } = await execFileAsync(JJ, args, { cwd: repoDir });
    return stdout;
  }
  const git = (args, cwd) => execFileAsync(GIT, args, { cwd });

  async function changeIdAt() {
    return (await jj(["log", "--no-graph", "-r", "@", "-T", "change_id"])).trim();
  }

  async function originMain() {
    const { stdout } = await execFileAsync(GIT, ["--git-dir", originDir, "rev-parse", "refs/heads/main"]);
    return stdout.trim();
  }

  const AUTH = "Basic " + Buffer.from(`skiff:${PASSWORD}`).toString("base64");
  const get = (p) => fetch(base + p, { headers: { Authorization: AUTH } });
  const post = (p, body) =>
    fetch(base + p, {
      method: "POST",
      headers: { Authorization: AUTH, "Content-Type": "application/json" },
      body: JSON.stringify(body ?? {}),
    });

  // Poll the change until it leaves `landing` (approve is async by design).
  async function settledChange(card) {
    for (let i = 0; i < 100; i++) {
      const change = await (await get(`/change/demo/${card}`)).json();
      if (change.state !== "landing") return change;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error(`change ${card} never left landing`);
  }

  // Round n+1 of a change: a new working-copy commit as a child of `onto`.
  async function makeRound(onto, file, content, message) {
    await jj(["new", ...(onto ? [onto] : [])]);
    await fs.writeFile(path.join(repoDir, file), content);
    await jj(["describe", "-m", message]);
    return changeIdAt();
  }

  before(async () => {
    tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-approve-"));
    await fs.cp(FIXTURES, tmp, { recursive: true });
    const jjConfig = path.join(tmp, "jj-config.toml");
    await fs.writeFile(jjConfig, '[user]\nname = "Test"\nemail = "test@example.invalid"\n');
    process.env.JJ_CONFIG = jjConfig;

    originDir = path.join(tmp, "origin.git");
    await git(["init", "--bare", originDir]);
    // The record repository (DW-003): a local checkout with a bare origin,
    // exactly the production shape.
    recordOriginDir = path.join(tmp, "record-origin.git");
    await git(["init", "--bare", recordOriginDir]);
    recordDir = path.join(tmp, "record");
    await git(["clone", recordOriginDir, recordDir]);
    const reposDir = path.join(tmp, "repos");
    repoDir = path.join(reposDir, "demo");
    await fs.mkdir(repoDir, { recursive: true });
    await jj(["git", "init", "--colocate"]);
    await git(["remote", "add", "origin", originDir], repoDir);
    await fs.writeFile(path.join(repoDir, "f.txt"), "base\n");
    await jj(["describe", "-m", "base"]);
    await jj(["bookmark", "create", "main", "-r", "@"]);
    await jj(["git", "push", "--remote", "origin", "--bookmark", "main"]);

    // A stub Fizzy: records every request, answers 201 like the real
    // comments endpoint.
    fizzyRequests = [];
    fizzyStub = http.createServer((req, res) => {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        fizzyRequests.push({ method: req.method, url: req.url, body, auth: req.headers.authorization });
        res.writeHead(201, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ id: "stub", body: { plain_text: "ok" } }));
      });
    });
    await new Promise((resolve) => fizzyStub.listen(0, "127.0.0.1", resolve));
    const tokenFile = path.join(tmp, "fizzy-token");
    await fs.writeFile(tokenFile, "stub-token\n", { mode: 0o600 });

    bridge = createBridge({
      password: PASSWORD,
      host: "127.0.0.1",
      port: 0,
      defaultCwd: tmp,
      pi: { sessionDir: tmp, binary: path.join(FIXTURES, "fake-pi.mjs") },
      muse: { sessionDir: path.join(tmp, "muse", "sessions"), binary: path.join(FIXTURES, "fake-muse.mjs") },
      opencode: { url: "http://127.0.0.1:1" },
      changes: {
        dir: path.join(tmp, "changes"),
        reposDir,
        binary: JJ,
        record: { dir: recordDir, binary: GIT },
        fizzy: {
          base: `http://127.0.0.1:${fizzyStub.address().port}`,
          account: "1",
          tokenFile,
        },
      },
    });
    await bridge.listen();
    base = `http://127.0.0.1:${bridge.port()}`;
  });

  after(async () => {
    await bridge?.close();
    await new Promise((resolve) => fizzyStub.close(resolve));
    delete process.env.JJ_CONFIG;
    await fs.rm(tmp, { recursive: true, force: true });
  });

  it("approve lands the rounds on origin/main and comments on the card, in that order", async () => {
    const r1 = await makeRound("main", "f.txt", "base\nfeature\n", "round 1");
    const r2 = await makeRound(null, "f.txt", "base\nfeature\npolish\n", "round 2");
    assert.equal(
      (await post("/change", { repo: "demo", card: 81, title: "pi model picker", session: "pi:multi-turn" })).status,
      201
    );
    await post("/change/demo/81/round", { author: "agent", changeId: r1, gatesRan: ["cargo test"] });
    await post("/change/demo/81/round", { author: "agent", changeId: r2 });
    await post("/change/demo/81/submit");

    const accepted = await post("/change/demo/81/approve");
    assert.equal(accepted.status, 202);
    assert.equal((await accepted.json()).state, "landing");
    // Approve is unavailable while landing — and a second approve later
    // needs in_review again, so both flavors answer 409.
    assert.equal((await post("/change/demo/81/approve")).status, 409);

    const change = await settledChange(81);
    assert.equal(change.state, "shipped");
    assert.equal(change.lastLanding.ok, true);
    assert.equal(await originMain(), change.landed.tip, "origin/main is the tip round's commit");
    assert.equal(change.cardComment.ok, true);
    assert.equal(fizzyRequests.length, 1);
    assert.equal(fizzyRequests[0].url, "/1/cards/81/comments.json");
    assert.match(fizzyRequests[0].body, /pi model picker/);
    assert.match(fizzyRequests[0].auth, /^Bearer stub-token$/);
    // The change ids survived the landing rebase — that is the point of
    // keeping change ids, not commit ids.
    assert.equal(change.rounds[0].changeId, r1);
    assert.equal(change.rounds[1].changeId, r2);
    // The record entry (DW-003): written, pushed, public-subset only.
    assert.equal(change.recordExport.ok, true);
    const entry = JSON.parse(await fs.readFile(path.join(recordDir, "demo", "81.json"), "utf8"));
    assert.equal(entry.title, "pi model picker");
    assert.equal(entry.tip, change.landed.tip);
    assert.equal(entry.rounds.length, 2);
    assert.match(entry.rounds[0].diff, /\+feature/);
    assert.deepEqual(entry.rounds[0].gatesRan, ["cargo test"]);
    assert.deepEqual(entry.afterward, []);
    assert.ok(!("note" in entry.rounds[0]), "round notes are private");
    assert.ok(!("session" in entry), "session ids are private");
    assert.ok(!("path" in entry), "filesystem paths are private");
    // Pushed: the record origin holds the commit.
    const { stdout: recordLog } = await execFileAsync(GIT, ["--git-dir", recordOriginDir, "log", "--oneline", "-1"]);
    assert.match(recordLog, /record: demo #81 — pi model picker/);
  });

  it("a conflicting landing returns to review carrying the conflicted rounds, and lands after resolution", async () => {
    // Rounds based on the ORIGINAL base, touching the same line the landed
    // change rewrote — the rebase onto the new main must conflict.
    const baseRev = (await jj(["log", "--no-graph", "-r", 'description(glob:"base*") & ~conflicts()', "-T", "change_id"])).trim();
    const c1 = await makeRound(baseRev, "f.txt", "base\nrival feature\n", "conflict round 1");
    await post("/change", { repo: "demo", card: 82 });
    await post("/change/demo/82/round", { author: "agent", changeId: c1 });
    await post("/change/demo/82/submit");
    await post("/change/demo/82/approve");

    const failed = await settledChange(82);
    assert.equal(failed.state, "in_review");
    assert.equal(failed.lastLanding.ok, false);
    assert.match(failed.lastLanding.reason, /conflicts; resolve it as the next round/);
    assert.deepEqual(failed.lastLanding.conflicts, [c1]);
    assert.equal(fizzyRequests.length, 1, "a failed landing writes nothing to the card");

    // The agent resolves the conflict in the conflicted round commit itself
    // (the landing rebase already rewrote it; the change id is stable), then
    // approves again.
    await jj(["edit", c1]);
    await fs.writeFile(path.join(repoDir, "f.txt"), "base\nfeature\npolish\nrival feature\n");
    await jj(["new"]); // move @ off the round before the bridge pushes it
    assert.equal((await jj(["log", "--no-graph", "-r", "conflicts()", "-T", "change_id"])).trim(), "");
    await post("/change/demo/82/approve");
    const shipped = await settledChange(82);
    assert.equal(shipped.state, "shipped");
    assert.equal(await originMain(), shipped.landed.tip);
    assert.equal(fizzyRequests.length, 2);
  });

  it("a push that keeps losing concedes back to review; removing the obstacle lets approve succeed", async () => {
    // A pre-receive hook that rejects everything stands in for a rival that
    // keeps landing first.
    const hook = path.join(originDir, "hooks", "pre-receive");
    await fs.writeFile(hook, "#!/bin/sh\necho rejected by test hook >&2\nexit 1\n", { mode: 0o755 });

    const shippedTip = (await jj(["log", "--no-graph", "-r", "main@origin", "-T", "change_id"])).trim();
    const p1 = await makeRound(shippedTip, "g.txt", "new file\n", "race round 1");
    await post("/change", { repo: "demo", card: 83 });
    await post("/change/demo/83/round", { author: "agent", changeId: p1 });
    await post("/change/demo/83/submit");
    await post("/change/demo/83/approve");

    const conceded = await settledChange(83);
    assert.equal(conceded.state, "in_review");
    assert.match(conceded.lastLanding.reason, /push lost the race 3 times/);

    await fs.rm(hook);
    await post("/change/demo/83/approve");
    const shipped = await settledChange(83);
    assert.equal(shipped.state, "shipped");
    assert.equal(await originMain(), shipped.landed.tip);
  });

  it("request-changes prompts the bound session and reopens the change", async () => {
    const tip = (await jj(["log", "--no-graph", "-r", "main@origin", "-T", "change_id"])).trim();
    const q1 = await makeRound(tip, "h.txt", "draft\n", "reviewable round");
    await post("/change", { repo: "demo", card: 84, session: "pi:multi-turn" });
    await post("/change/demo/84/round", { author: "agent", changeId: q1 });

    // Only in_review changes take requests.
    assert.equal((await post("/change/demo/84/request_changes", { note: "tighten this" })).status, 409);
    await post("/change/demo/84/submit");
    assert.equal((await post("/change/demo/84/request_changes", { note: "" })).status, 400);

    const response = await post("/change/demo/84/request_changes", { note: "tighten the error copy" });
    assert.equal(response.status, 200);
    const change = await response.json();
    assert.equal(change.state, "working");
    assert.equal(change.lastRequest.note, "tighten the error copy");

    // The note reached the fake pi session through the prompt surface.
    for (let i = 0; i < 50; i++) {
      const messages = await (await get("/session/pi:multi-turn/message")).json();
      if (JSON.stringify(messages).includes("tighten the error copy")) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.fail("the request-changes note never reached the session");
  });

  it("rejects a request when no session is bound, and validates session bindings", async () => {
    const tip = (await jj(["log", "--no-graph", "-r", "main@origin", "-T", "change_id"])).trim();
    const s1 = await makeRound(tip, "i.txt", "x\n", "unbound round");
    await post("/change", { repo: "demo", card: 85 });
    await post("/change/demo/85/round", { author: "agent", changeId: s1 });
    await post("/change/demo/85/submit");
    const unbound = await post("/change/demo/85/request_changes", { note: "hello?" });
    assert.equal(unbound.status, 409);
    assert.match((await unbound.json()).error, /no bound session/);

    assert.equal((await post("/change/demo/85/session", { session: "warp:123" })).status, 400);
    assert.equal((await post("/change/demo/85/session", { session: "not-qualified" })).status, 400);
    const bound = await post("/change/demo/85/session", { session: "pi:multi-turn" });
    assert.equal(bound.status, 200);
    assert.equal((await bound.json()).session, "pi:multi-turn");
    assert.equal((await post("/change", { repo: "demo", card: 86, session: "warp:123" })).status, 400);
  });

  it("a failed record export is recorded on the change and never un-ships it", async () => {
    const hook = path.join(recordOriginDir, "hooks", "pre-receive");
    await fs.writeFile(hook, "#!/bin/sh\necho record origin rejects >&2\nexit 1\n", { mode: 0o755 });

    const tip = (await jj(["log", "--no-graph", "-r", "main@origin", "-T", "change_id"])).trim();
    const w1 = await makeRound(tip, "j.txt", "content\n", "export-failure round");
    await post("/change", { repo: "demo", card: 87 });
    await post("/change/demo/87/round", { author: "agent", changeId: w1 });
    await post("/change/demo/87/submit");
    await post("/change/demo/87/approve");

    const change = await settledChange(87);
    await fs.rm(hook);
    assert.equal(change.state, "shipped", "the land is the irreversible half; the export never blocks it");
    assert.equal(change.recordExport.ok, false);
    assert.match(change.recordExport.message, /record export failed/);
    assert.equal(change.cardComment.ok, true, "the card comment still happens after a failed export");
  });

  it("approve requires review", async () => {
    assert.equal((await post("/change/demo/86", {})).status, 404);
    await post("/change", { repo: "demo", card: 86 });
    assert.equal((await post("/change/demo/86/approve")).status, 409);
  });
});
