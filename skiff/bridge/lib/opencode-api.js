// bridge/lib/opencode-api.js
// Minimal HTTP client for a headless `opencode serve` (verified against
// opencode 1.18.15). The serve process is a sibling service on loopback —
// deploy/opencode-serve.service — and speaks its own JSON API with no auth;
// every failure here becomes an HttpError(502) so the bridge's callers
// surface one consistent error shape.
//
// Endpoints used (of the many opencode exposes):
//   GET    /session                  list (includes child sessions; callers
//                                    filter parentID)
//   POST   /session {title}          create
//   GET    /session/{id}             one session (404 -> null)
//   PATCH  /session/{id} {title}     rename
//   GET    /session/{id}/message     [{ info, parts }] — the shape skiff's
//                                    transcript is built from (skiff's own
//                                    wire shape descends from it)
//   POST   /session/{id}/prompt_async { parts: [{type:"text",text}] }
//   POST   /session/{id}/abort       interrupt the running prompt
//   GET    /event                    the SSE event bus (message.updated,
//                                    message.part.updated, session.idle, …)

import { HttpError } from "./errors.js";

export function createOpencodeApi(baseUrl) {
  async function request(method, path, body = undefined, { allow404 = false } = {}) {
    let response;
    try {
      response = await fetch(baseUrl + path, {
        method,
        headers: body !== undefined ? { "Content-Type": "application/json" } : {},
        body: body !== undefined ? JSON.stringify(body) : undefined,
      });
    } catch (err) {
      throw new HttpError(502, `opencode serve unreachable: ${err.cause?.code ?? err.message}`);
    }
    if (allow404 && response.status === 404) return null;
    if (!response.ok) {
      const snippet = (await response.text().catch(() => "")).slice(0, 120);
      throw new HttpError(502, `opencode serve answered HTTP ${response.status}${snippet ? `: ${snippet}` : ""}`);
    }
    if (response.status === 204) return null;
    try {
      return await response.json();
    } catch (err) {
      throw new HttpError(502, `opencode serve sent invalid JSON: ${err.message}`);
    }
  }

  return {
    sessions: () => request("GET", "/session"),
    session: (id) => request("GET", `/session/${id}`, undefined, { allow404: true }),
    createSession: (title) => request("POST", "/session", { title }),
    rename: (id, title) => request("PATCH", `/session/${id}`, { title }),
    messages: (id) => request("GET", `/session/${id}/message`),
    promptAsync: (id, parts) => request("POST", `/session/${id}/prompt_async`, { parts }),
    abort: (id) => request("POST", `/session/${id}/abort`),

    // Subscribe to the event bus. opencode frames the stream as SSE-style
    // "data: {json}" lines; each parsed event object is handed to onEvent.
    // Returns { close }; onClose fires once when the stream ends for any
    // reason other than close() — the caller owns reconnection.
    subscribeEvents(onEvent, onClose) {
      const controller = new AbortController();
      let closed = false;
      fetch(baseUrl + "/event", { signal: controller.signal })
        .then((response) => {
          if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
          const reader = createEventReader(response.body, onEvent);
          return reader;
        })
        .catch(() => {})
        .finally(() => {
          if (!closed) onClose();
        });
      return {
        close() {
          closed = true;
          controller.abort();
        },
      };
    },
  };
}

// Drain a web ReadableStream of SSE frames, invoking onEvent per parsed
// "data:" payload. Resolves when the stream ends.
async function createEventReader(body, onEvent) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) return;
    buffer += decoder.decode(value, { stream: true });
    let nl;
    while ((nl = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, nl).replace(/\r$/, "");
      buffer = buffer.slice(nl + 1);
      if (!line.startsWith("data: ")) continue;
      try {
        onEvent(JSON.parse(line.slice("data: ".length)));
      } catch {
        // a partial or non-JSON data line is not worth surfacing
      }
    }
  }
}
