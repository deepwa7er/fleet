// bridge/test/deploy.test.js
// Approve = land + ship (card #123): a real bridge, a real temp jj repo with
// a bare git origin, and a stub tugboat daemon standing in for the deploy
// engine. Covers the trigger (jobs recorded on the change), the outcome poll
// (ok and failed jobs both append), a daemon that is down (recorded, never
// un-ships), the willDeploy preview, and the token gate (no token → the
// bridge never touches the daemon).
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
import { createTugboatClient } from "../lib/tugboat.js";

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

describe("approve triggers the full fleet deploy", { skip: SKIP }, () => {
  let tmp;
  let repoDir;
  let reposDir;
  let originDir;
  let recordDir;
  let recordOriginDir;
  let bridge;
  let base;
  let fizzyStub;
  let tugboatStub;
  let tugboatState;
  let tokenFile;

  async function jj(args) {
    const { stdout } = await execFileAsync(JJ, args, { cwd: repoDir });
    return stdout;
  }
  const git = (args, cwd) => execFileAsync(GIT, args, { cwd });

  async function changeIdAt() {
    return (await jj(["log", "--no-graph", "-r", "@", "-T", "change_id"])).trim();
  }

  const AUTH = "Basic " + Buffer.from(`skiff:${PASSWORD}`).toString("base64");
  const get = (p, target = base) => fetch(target + p, { headers: { Authorization: AUTH } });
  const post = (p, body, target = base) =>
    fetch(target + p, {
      method: "POST",
      headers: { Authorization: AUTH, "Content-Type": "application/json" },
      body: JSON.stringify(body ?? {}),
    });

  async function settledChange(card, target = base) {
    for (let i = 0; i < 100; i++) {
      const change = await (await get(`/change/demo/${card}`, target)).json();
      if (change.state !== "landing") return change;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error(`change ${card} never left landing`);
  }

  // Wait until the triggered deploy has a terminal story on the change: the
  // whole trigger failed, or every *started* job (one with a job id) has an
  // outcome. Entries reported in_progress by the daemon have no job and no
  // outcome by construction.
  async function deploySettled(card) {
    for (let i = 0; i < 200; i++) {
      const change = await (await get(`/change/demo/${card}`)).json();
      const deploy = change.deploy;
      if (deploy?.error) return change;
      const started = deploy?.services.filter((s) => s.jobId !== null) ?? [];
      if (deploy && deploy.services.length > 0 && started.every((s) => s.outcome !== null)) {
        return change;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(`deploy for ${card} never settled`);
  }

  async function makeRound(onto, file, content, message) {
    await jj(["new", ...(onto ? [onto] : [])]);
    await fs.writeFile(path.join(repoDir, file), content);
    await jj(["describe", "-m", message]);
    return changeIdAt();
  }

  // Create, round, submit, approve a fresh change; returns the settled change.
  async function approveChange(card, { file = "f.txt", base = null } = {}) {
    const onto = base ?? (await jj(["log", "--no-graph", "-r", "main@origin", "-T", "change_id"])).trim();
    const round = await makeRound(onto, file, `content ${card}\n`, `round for ${card}`);
    await post("/change", { repo: "demo", card, title: `deploy test ${card}` });
    await post(`/change/demo/${card}/round`, { author: "agent", changeId: round, gatesRan: ["node --test"] });
    await post(`/change/demo/${card}/submit`);
    await post(`/change/demo/${card}/approve`);
    return settledChange(card);
  }

  before(async () => {
    tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-deploy-"));
    await fs.cp(FIXTURES, tmp, { recursive: true });
    const jjConfig = path.join(tmp, "jj-config.toml");
    await fs.writeFile(jjConfig, '[user]\nname = "Test"\nemail = "test@example.invalid"\n');
    process.env.JJ_CONFIG = jjConfig;

    originDir = path.join(tmp, "origin.git");
    await git(["init", "--bare", originDir]);
    recordOriginDir = path.join(tmp, "record-origin.git");
    await git(["init", "--bare", recordOriginDir]);
    recordDir = path.join(tmp, "record");
    await git(["clone", recordOriginDir, recordDir]);
    reposDir = path.join(tmp, "repos");
    repoDir = path.join(reposDir, "demo");
    await fs.mkdir(repoDir, { recursive: true });
    await jj(["git", "init", "--colocate"]);
    await git(["remote", "add", "origin", originDir], repoDir);
    await fs.writeFile(path.join(repoDir, "f.txt"), "base\n");
    await jj(["describe", "-m", "base"]);
    await jj(["bookmark", "create", "main", "-r", "@"]);
    await jj(["git", "push", "--remote", "origin", "--bookmark", "main"]);

    fizzyStub = http.createServer((req, res) => {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        res.writeHead(201, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ id: "stub", body: { plain_text: "ok" } }));
      });
    });
    await new Promise((resolve) => fizzyStub.listen(0, "127.0.0.1", resolve));
    tokenFile = path.join(tmp, "fizzy-token");
    await fs.writeFile(tokenFile, "stub-token\n", { mode: 0o600 });

    // The stub tugboat daemon. `jobs` is what /deploy returns; each job's
    // /jobs/{id} reports `outcome` once its pollsSeen count exceeds
    // `pollsUntilDone`. Set `down` to a message to make every request fail.
    tugboatState = {
      jobs: [],
      services: [{ name: "a" }, { name: "b" }, { name: "c" }],
      deployCalls: 0,
      down: null,
    };
    tugboatStub = http.createServer((req, res) => {
      const send = (body, status = 200) => {
        res.writeHead(status, { "Content-Type": "application/json" });
        res.end(JSON.stringify(body));
      };
      if (tugboatState.down) {
        return send({ error: tugboatState.down }, 503);
      }
      const pathname = new URL(req.url, "http://stub").pathname;
      if (req.method === "POST" && pathname === "/deploy") {
        tugboatState.deployCalls += 1;
        return send({ jobs: tugboatState.jobs });
      }
      if (req.method === "GET" && pathname === "/services") {
        return send(tugboatState.services);
      }
      if (req.method === "GET" && pathname.startsWith("/jobs/")) {
        const id = decodeURIComponent(pathname.slice("/jobs/".length));
        const job = tugboatState.jobs.find((j) => j.job_id === id);
        if (!job) return send({ error: `no such job ${id}` }, 404);
        job.pollsSeen = (job.pollsSeen ?? 0) + 1;
        const outcome = job.pollsSeen > (job.pollsUntilDone ?? 0) ? job.outcome : null;
        return send({ id, outcome });
      }
      return send({ error: "not found" }, 404);
    });
    await new Promise((resolve) => tugboatStub.listen(0, "127.0.0.1", resolve));

    const changesConfig = {
      dir: path.join(tmp, "changes"),
      reposDir,
      binary: JJ,
      record: { dir: recordDir, binary: GIT },
      fizzy: {
        base: `http://127.0.0.1:${fizzyStub.address().port}`,
        account: "1",
        tokenFile,
      },
    };
    bridge = createBridge({
      password: PASSWORD,
      host: "127.0.0.1",
      port: 0,
      defaultCwd: tmp,
      pi: { sessionDir: tmp, binary: path.join(FIXTURES, "fake-pi.mjs") },
      muse: { sessionDir: path.join(tmp, "muse", "sessions"), binary: path.join(FIXTURES, "fake-muse.mjs") },
      opencode: { url: "http://127.0.0.1:1" },
      changes: {
        ...changesConfig,
        tugboat: {
          url: `http://127.0.0.1:${tugboatStub.address().port}`,
          token: "stub-token",
          pollIntervalMs: 50,
          pollDeadlineMs: 5_000,
        },
      },
    });
    await bridge.listen();
    base = `http://127.0.0.1:${bridge.port()}`;
  });

  after(async () => {
    await bridge?.close();
    await new Promise((resolve) => fizzyStub.close(resolve));
    await new Promise((resolve) => tugboatStub.close(resolve));
    delete process.env.JJ_CONFIG;
    await fs.rm(tmp, { recursive: true, force: true });
  });

  it("approve records the triggered jobs and polls their outcomes onto the change", async () => {
    tugboatState.jobs = [
      { name: "lighthouse", job_id: "lighthouse-1", outcome: { ok: true, error: null }, pollsUntilDone: 0 },
      { name: "tidepool", job_id: "tidepool-2", outcome: { ok: false, error: "build failed" }, pollsUntilDone: 0 },
    ];
    tugboatState.deployCalls = 0;

    const change = await approveChange(91);
    assert.equal(change.state, "shipped");
    assert.equal(tugboatState.deployCalls, 1, "exactly one fleet deploy per approval");

    const settled = await deploySettled(91);
    const deploy = settled.deploy;
    assert.equal(deploy.error, null);
    assert.equal(deploy.services.length, 2);
    assert.deepEqual(
      deploy.services.map((s) => s.name),
      ["lighthouse", "tidepool"]
    );
    assert.equal(deploy.services[0].status, "started");
    assert.deepEqual(deploy.services[0].outcome, { ok: true });
    assert.equal(deploy.services[1].outcome.ok, false);
    assert.equal(deploy.services[1].outcome.message, "build failed");

    // The deploy is metadata: the land, record export, and card comment all
    // still happened around it.
    assert.equal(settled.recordExport.ok, true);
    assert.equal(settled.cardComment.ok, true);
  });

  it("a service already deploying is reported in_progress and never polled", async () => {
    tugboatState.jobs = [
      { name: "breakwater", job_id: "breakwater-1", outcome: { ok: true, error: null }, pollsUntilDone: 0 },
      { name: "sonar", status: "in_progress" },
    ];
    const change = await approveChange(92);
    const settled = await deploySettled(92);
    const [breakwater, sonar] = settled.deploy.services;
    assert.equal(breakwater.outcome.ok, true);
    assert.equal(sonar.jobId, null);
    assert.equal(sonar.status, "in_progress");
    assert.equal(sonar.outcome, null, "no job was started, so nothing is polled");
  });

  it("a daemon that is down is recorded on the change and never un-ships it", async () => {
    tugboatState.down = "daemon restarting";
    try {
      const change = await approveChange(93);
      assert.equal(change.state, "shipped", "the land is the irreversible half");
      const settled = await deploySettled(93);
      assert.match(settled.deploy.error, /answered 503|unreachable/);
      assert.equal(settled.cardComment.ok, true, "the rest of the metadata still happens");
    } finally {
      tugboatState.down = null;
    }
  });

  it("the desk sees what an approval will deploy", async () => {
    tugboatState.jobs = [];
    const change = await approveChange(94);
    assert.deepEqual(change.willDeploy, { services: 3 });
  });

  it("an already-deploying service does not block the others or double-trigger", async () => {
    tugboatState.jobs = [{ name: "sonar", status: "in_progress" }];
    tugboatState.deployCalls = 0;
    const change = await approveChange(95);
    const settled = await deploySettled(95);
    assert.equal(tugboatState.deployCalls, 1);
    assert.equal(settled.deploy.services[0].status, "in_progress");
  });

  it("without a token the bridge never touches the daemon", async () => {
    const plain = createBridge({
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
        fizzy: { base: `http://127.0.0.1:${fizzyStub.address().port}`, account: "1", tokenFile },
        tugboat: { url: `http://127.0.0.1:${tugboatStub.address().port}` }, // no token
      },
    });
    await plain.listen();
    const plainBase = `http://127.0.0.1:${plain.port()}`;
    try {
      // The change is created and reviewed on either bridge (they share the
      // store); only approve must go through the tokenless one.
      const onto = (await jj(["log", "--no-graph", "-r", "main@origin", "-T", "change_id"])).trim();
      const round = await makeRound(onto, "f2.txt", "content 96\n", "round for 96");
      await post("/change", { repo: "demo", card: 96, title: "deploy test 96" });
      await post(`/change/demo/96/round`, { author: "agent", changeId: round, gatesRan: ["node --test"] });
      await post(`/change/demo/96/submit`);
      tugboatState.deployCalls = 0;
      await post(`/change/demo/96/approve`, {}, plainBase);

      const change = await settledChange(96, plainBase);
      assert.equal(change.state, "shipped");
      assert.equal(change.deploy, null, "no token → no deploy record");
      assert.equal(change.willDeploy, null, "no token → no preview");
      assert.equal(tugboatState.deployCalls, 0, "the daemon was never called");
    } finally {
      await plain.close();
    }
  });
});

describe("tugboat client", () => {
  it("no token disables the client entirely", () => {
    assert.equal(createTugboatClient({ token: null }), null);
    assert.equal(createTugboatClient({}), null);
  });
});
