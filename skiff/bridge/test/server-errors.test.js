// bridge/test/server-errors.test.js
// Failure-path HTTP tests: a second bridge whose fake pi is told to refuse
// prompts and new_session. The fake reads its env at spawn, so these flags
// must be set before this file boots its bridge — and only in this file,
// which runs in its own process under node --test.
process.env.FAKE_PI_PROMPT_ERROR = "1";
process.env.FAKE_PI_FAIL_NEW_SESSION = "1";

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

let bridge;
let base;
let tmp;

before(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "skiff-bridge-errors-"));
  await fs.cp(FIXTURES, tmp, { recursive: true });
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
});

after(async () => {
  await bridge.close();
  await fs.rm(tmp, { recursive: true, force: true });
});

const AUTH = "Basic " + Buffer.from(`opencode:${PASSWORD}`).toString("base64");

function post(p, body = undefined) {
  return fetch(base + p, {
    method: "POST",
    headers: { Authorization: AUTH, ...(body !== undefined ? { "Content-Type": "application/json" } : {}) },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
}

describe("bridge failure paths", () => {
  it("surfaces a rejected prompt as 502 with the pi error", async () => {
    const response = await post("/session/multi-turn/prompt_async", { parts: [{ type: "text", text: "boom" }] });
    assert.equal(response.status, 502);
    const body = await response.json();
    assert.match(body.error, /prompt refused/);
  });

  it("surfaces a failed create as 502 and kills the child", async () => {
    const response = await post("/session", { title: "will fail" });
    assert.equal(response.status, 502);
    const body = await response.json();
    assert.match(body.error, /create session failed/);
  });
});
