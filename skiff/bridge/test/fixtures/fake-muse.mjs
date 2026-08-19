#!/usr/bin/env node
// fake-muse.mjs — a scripted stand-in for `muse exec --json` that drives the
// bridge's real code paths in tests without an LLM. It implements just
// enough of muse's observed behavior (Muse Code 0.2.1, verified live before
// writing this):
//
// - The session dir resolves via XDG_DATA_HOME (<root>/muse/sessions), the
//   same mechanism the muse harness configures for spawned children.
// - The first run on a session id creates sessions/YYYY/MM/DD/<id>/ and
//   writes the metadata + automatic-name records; later runs append.
// - stdout carries the envelope records: a command acknowledgment first
//   (the harness's readiness signal), then INCREMENTAL run.output.delta
//   chunks, then run.terminal.completed. Committed transcript events go to
//   the session FILE only, never stdout — exactly like real muse.
// - The assistant reply echoes the prompt text (like fake-pi), committed to
//   the file as assistant_message_committed after the deltas finish.
// - SIGINT mid-run kills the process without a terminal record anywhere —
//   real muse behaves the same, and the bridge's exit handling owns
//   convergence.
//
// Env hooks for failure-path tests:
//   FAKE_MUSE_DELAY_MS   per-delta streaming delay (default 40)
//   FAKE_MUSE_FAIL=1     refuse to run: one stderr line, exit 1, no stdout
//                        record (the harness surfaces the stderr as a 502)
//   FAKE_MUSE_TOOLS=1    commit a tool-call batch + results to the file
//                        before the assistant text (mid-run tool traffic)
//
// The file guards against being executed as a test: `node --test` discovers
// any .mjs under a directory named "test", and running the fake as one must
// not block.

import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { randomUUID } from "node:crypto";

if (process.argv[1] && path.basename(process.argv[1]) === "fake-muse.mjs" && process.argv[2] !== "exec") {
  process.exit(0); // discovered by node --test (or run bare): do nothing
}

const args = process.argv.slice(2);
function flagValue(name) {
  const index = args.indexOf(name);
  return index === -1 ? null : args[index + 1];
}

const sessionId = flagValue("--session-id");
const promptFile = flagValue("--prompt-file");
if (!sessionId || !promptFile) {
  process.stderr.write("fake-muse: --session-id and --prompt-file are required\n");
  process.exit(2);
}

if (process.env.FAKE_MUSE_FAIL === "1") {
  process.stderr.write("fake-muse: refusing to run (FAKE_MUSE_FAIL)\n");
  process.exit(1);
}

const prompt = fs.readFileSync(promptFile, "utf8");
const delayMs = Number(process.env.FAKE_MUSE_DELAY_MS ?? 40);

const dataHome =
  process.env.XDG_DATA_HOME && process.env.XDG_DATA_HOME.trim() !== ""
    ? process.env.XDG_DATA_HOME
    : path.join(os.homedir(), ".local", "share");
const now = new Date();
const pad = (n) => String(n).padStart(2, "0");
const sessionDir = path.join(
  dataHome,
  "muse",
  "sessions",
  String(now.getFullYear()),
  pad(now.getMonth() + 1),
  pad(now.getDate()),
  sessionId
);
const sessionFile = path.join(sessionDir, "session.jsonl");

let sequence = 0;
if (fs.existsSync(sessionFile)) {
  sequence = fs.readFileSync(sessionFile, "utf8").split("\n").filter(Boolean).length;
}

function envelope(payloadType, payload) {
  return {
    schema_version: 1,
    id: randomUUID(),
    stream: { kind: "session", id: sessionId },
    sequence: ++sequence,
    recorded_at: Date.now() * 1000,
    record_type: "event",
    durability: "durable",
    causation_id: null,
    payload_type: payloadType,
    payload_schema_version: 1,
    payload,
  };
}

function appendRecord(payloadType, payload) {
  fs.appendFileSync(sessionFile, JSON.stringify(envelope(payloadType, payload)) + "\n");
}

function emit(payloadType, payload) {
  process.stdout.write(JSON.stringify(envelope(payloadType, payload)) + "\n");
}

const runId = randomUUID();
function runRecord(event) {
  appendRecord("runtime.session", { kind: "run", run_id: runId, event });
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function main() {
  if (!fs.existsSync(sessionFile)) {
    fs.mkdirSync(sessionDir, { recursive: true });
    appendRecord("runtime.session.metadata", {
      kind: "metadata",
      record: { model_id: "fake-muse-1", provider_id: "meta", workspace_root: process.cwd() },
    });
    appendRecord("session.name.changed", {
      new_name: `fake-${sessionId.slice(0, 8)}`,
      previous_name: null,
      source: "automatic",
    });
  }

  emit("runtime.command.accepted", { kind: "command_accepted", command_id: runId, command_kind: "turn.submit" });
  runRecord({ kind: "started", prompt });

  if (process.env.FAKE_MUSE_TOOLS === "1") {
    const callId = `call_${runId.slice(0, 8)}`;
    runRecord({
      kind: "assistant_tool_calls_committed",
      message_id: randomUUID(),
      tool_calls: [{ args: '{"cmd":"true"}', call_id: callId, id: `fc_${runId.slice(0, 8)}`, name: "bash" }],
    });
    await sleep(delayMs);
    runRecord({
      kind: "tool_result_batch_committed",
      batch_id: randomUUID(),
      results: [{ text: "tool output", tool_call_id: callId, tool_call_index: 0 }],
    });
  }

  // Stream the reply as incremental chunks, like real muse.
  const chunkSize = Math.max(1, Math.ceil(prompt.length / 5));
  for (let at = 0; at < prompt.length; at += chunkSize) {
    await sleep(delayMs);
    emit("run.output.delta", { kind: "run_output_delta", text: prompt.slice(at, at + chunkSize) });
  }

  await sleep(delayMs);
  runRecord({ kind: "assistant_message_committed", message_id: randomUUID(), text: prompt });
  runRecord({ kind: "terminal", reason: null, terminal: "completed" });
  emit("run.terminal.completed", { kind: "run_terminal", terminal: "completed", reason: null, text: prompt });
}

main();
