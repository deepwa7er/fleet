// fake-opencode.mjs — an in-process stand-in for `opencode serve`,
// implementing just enough of its HTTP API (verified against opencode
// 1.18.15's OpenAPI document) for the opencode harness's code paths:
// session CRUD with opencode's own shapes (slug/title/parentID/time),
// { info, parts } message lists, prompt_async driving a scripted streaming
// run, abort, and the /event SSE bus (data: {json} frames — server.connected
// first, then message.updated / message.part.updated / session.idle).
//
// Imported by tests (createFakeOpencode), never spawned: the harness talks
// HTTP, so an in-process node:http server exercises exactly the same code.

import http from "node:http";
import { randomUUID } from "node:crypto";

export function createFakeOpencode({ streamDelayMs = 60 } = {}) {
  const sessions = new Map(); // id -> opencode session object
  const messages = new Map(); // id -> [{ info, parts }]
  const subscribers = new Set(); // /event responses
  const timers = new Set();
  let counter = 0;

  function makeSession({ title = "", slug, parentID, directory = "/home/deepwater/code" } = {}) {
    const id = `ses_fake${String(++counter).padStart(4, "0")}`;
    const now = Date.now();
    const session = {
      id,
      slug: slug ?? `slug-${counter}`,
      projectID: "global",
      directory,
      title,
      version: "1.18.15",
      time: { created: now, updated: now },
      ...(parentID ? { parentID } : {}),
    };
    sessions.set(id, session);
    messages.set(id, []);
    return session;
  }

  function emit(type, properties) {
    const frame = `data: ${JSON.stringify({ id: `evt_${randomUUID()}`, type, properties })}\n\n`;
    for (const res of subscribers) {
      if (!res.writableEnded) res.write(frame);
    }
  }

  function json(res, status, body) {
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(JSON.stringify(body));
  }

  function readBody(req) {
    return new Promise((resolve) => {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => {
        const raw = Buffer.concat(chunks).toString("utf8");
        resolve(raw === "" ? null : JSON.parse(raw));
      });
    });
  }

  // A scripted run: user message lands at once; the assistant message grows
  // its text part chunk by chunk (message.part.updated per chunk), then
  // completes (message.updated with time.completed, session.idle). Abort
  // completes it immediately with whatever streamed.
  function startRun(sessionID, text) {
    const list = messages.get(sessionID);
    const now = Date.now();
    list.push({
      info: { id: `msg_u${++counter}`, role: "user", sessionID, time: { created: now } },
      parts: [{ id: `prt_u${counter}`, type: "text", text, sessionID, messageID: `msg_u${counter}` }],
    });
    const assistant = {
      info: { id: `msg_a${++counter}`, role: "assistant", sessionID, modelID: "fake-oc-model", time: { created: now } },
      parts: [{ id: `prt_a${counter}`, type: "text", text: "", sessionID, messageID: `msg_a${counter}` }],
    };
    list.push(assistant);
    sessions.get(sessionID).time.updated = now;
    emit("message.updated", { sessionID, info: assistant.info });

    const chunkSize = Math.max(1, Math.ceil(text.length / 5));
    let at = 0;
    const finish = () => {
      clearInterval(timer);
      timers.delete(timer);
      assistant.info.time.completed = Date.now();
      emit("message.updated", { sessionID, info: assistant.info });
      emit("session.idle", { sessionID });
      runs.delete(sessionID);
    };
    const timer = setInterval(() => {
      if (at >= text.length) {
        finish();
        return;
      }
      assistant.parts[0].text += text.slice(at, at + chunkSize);
      at += chunkSize;
      emit("message.part.updated", { sessionID, part: assistant.parts[0], time: Date.now() });
    }, streamDelayMs);
    timers.add(timer);
    runs.set(sessionID, finish);
  }
  const runs = new Map(); // sessionID -> finish()

  const server = http.createServer(async (req, res) => {
    const { pathname } = new URL(req.url, "http://localhost");
    const parts = pathname.split("/").filter(Boolean);

    if (req.method === "GET" && pathname === "/event") {
      res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
      res.write(`data: ${JSON.stringify({ id: "evt_hello", type: "server.connected", properties: {} })}\n\n`);
      subscribers.add(res);
      res.on("close", () => subscribers.delete(res));
      return;
    }
    if (req.method === "GET" && pathname === "/session") {
      return json(res, 200, [...sessions.values()]);
    }
    if (req.method === "POST" && pathname === "/session") {
      const body = await readBody(req);
      return json(res, 200, makeSession({ title: body?.title ?? "" }));
    }
    if (parts[0] === "session" && parts.length >= 2) {
      const session = sessions.get(parts[1]);
      if (!session) return json(res, 404, { error: "not found" });
      if (req.method === "GET" && parts.length === 2) return json(res, 200, session);
      if (req.method === "PATCH" && parts.length === 2) {
        const body = await readBody(req);
        if (typeof body?.title === "string") session.title = body.title;
        return json(res, 200, session);
      }
      if (req.method === "GET" && parts[2] === "message") return json(res, 200, messages.get(session.id));
      if (req.method === "POST" && parts[2] === "prompt_async") {
        const body = await readBody(req);
        const text = (body?.parts ?? []).map((p) => p.text ?? "").join("\n");
        startRun(session.id, text);
        return json(res, 200, { ok: true });
      }
      if (req.method === "POST" && parts[2] === "abort") {
        runs.get(session.id)?.();
        return json(res, 200, true);
      }
    }
    return json(res, 404, { error: "not found" });
  });

  return {
    server,
    sessions,
    messages,
    makeSession,
    listen() {
      return new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    },
    url() {
      return `http://127.0.0.1:${server.address().port}`;
    },
    close() {
      for (const timer of timers) clearInterval(timer);
      for (const res of subscribers) res.end();
      return new Promise((resolve) => server.close(resolve));
    },
  };
}
