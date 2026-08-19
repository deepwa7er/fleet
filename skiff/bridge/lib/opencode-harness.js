// bridge/lib/opencode-harness.js
// The opencode harness: skiff sessions backed by a headless `opencode serve`
// (deploy/opencode-serve.service, loopback). Unlike pi and muse, opencode is
// itself a server that owns its sessions end to end, so this adapter is a
// translation layer: session and message shapes map nearly 1:1 (skiff's
// { info, parts } wire shape descends from opencode's), prompts and aborts
// pass through, and the live stream is driven by opencode's own event bus.
//
// Streaming model: no overlay. opencode updates the in-flight assistant
// message IN its message list (parts stream via message.part.* events), so
// the driver just refetches the transcript — coalesced, like the overlay
// flushes — and lets the registry's diff emit the replaces. Working state is
// derived the same way skiff's first paint derives it: the newest assistant
// message without time.completed is a run in flight.
//
// Capabilities: rename (PATCH /session/{id} {title}); no orchestrator (a pi
// extension). Child sessions (parentID set — subagent tasks) are run
// internals and never listed.

import { createOpencodeApi } from "./opencode-api.js";
import { HttpError } from "./errors.js";

// Refetch coalescing for event bursts (a streaming part fires
// message.part.delta at token frequency), matching the registry's overlay
// flush cadence.
const REFRESH_INTERVAL_MS = 100;
// A dropped event-bus connection (opencode serve restarting) retries after a
// beat; each reconnect starts with a full refetch, so the stream converges.
const RECONNECT_DELAY_MS = 1000;

export const DEFAULT_OPENCODE_URL = "http://127.0.0.1:4130";

function mapSession(session) {
  return {
    id: session.id,
    title: session.title?.trim() ? session.title : session.slug ?? null,
    directory: session.directory ?? null,
    time: { created: session.time?.created ?? null, updated: session.time?.updated ?? null },
    model: typeof session.model?.id === "string" ? { id: session.model.id } : null,
  };
}

function mapMessage({ info, parts }) {
  const mappedInfo = {
    id: info.id,
    role: info.role,
    time: { created: info.time?.created ?? null },
  };
  if (info.role === "assistant") {
    mappedInfo.agent = typeof info.modelID === "string" ? info.modelID : null;
    if (info.time?.completed !== undefined) mappedInfo.time.completed = info.time.completed;
  }
  const mappedParts = [];
  for (const part of parts) {
    switch (part.type) {
      case "text":
        mappedParts.push({ type: "text", text: part.text ?? "", id: part.id, ...(part.synthetic ? { synthetic: true } : {}) });
        break;
      case "reasoning":
        mappedParts.push({ type: "reasoning", text: part.text ?? "", id: part.id });
        break;
      case "tool": {
        const status = part.state?.status ?? "running";
        const state = { status };
        if (typeof part.state?.output === "string") state.output = part.state.output;
        if (status === "error" && typeof part.state?.error === "string") state.output = part.state.error;
        if (typeof part.state?.title === "string") state.title = part.state.title;
        mappedParts.push({ type: "tool", tool: part.tool ?? "", id: part.callID ?? part.id, state });
        break;
      }
      case "file":
        mappedParts.push({ type: "file", filename: part.filename ?? null, id: part.id });
        break;
      default:
        break; // step-start/step-finish/snapshot/patch/… render nothing
    }
  }
  return { info: mappedInfo, parts: mappedParts };
}

// A run in flight leaves the newest assistant message without
// time.completed — the same rule skiff's first paint uses.
function derivedWorking(messages) {
  const newest = messages[messages.length - 1];
  return Boolean(newest && newest.info.role === "assistant" && newest.info.time.completed === undefined);
}

export function createOpencodeHarness(config) {
  const api = createOpencodeApi(config.url ?? process.env.OPENCODE_SERVE_URL ?? DEFAULT_OPENCODE_URL);

  async function fetchMappedMessages(localId) {
    return (await api.messages(localId)).map(mapMessage);
  }

  return {
    name: "opencode",
    capabilities: { rename: true, orchestrator: false, model: false },

    async listSessions() {
      const sessions = await api.sessions();
      return sessions.filter((s) => !s.parentID).map(mapSession);
    },

    async getSession(localId) {
      const session = await api.session(localId);
      return session ? mapSession(session) : null;
    },

    async getMessages(localId) {
      const session = await api.session(localId);
      if (!session) return null;
      return fetchMappedMessages(localId);
    },

    async createSession({ title }) {
      const session = await api.createSession(title);
      if (typeof session?.id !== "string") throw new HttpError(502, "opencode serve did not return a session id");
      return session.id;
    },

    async promptAsync(localId, text) {
      const session = await api.session(localId);
      if (!session) throw new HttpError(404, `session opencode:${localId} not found`);
      await api.promptAsync(localId, [{ type: "text", text }]);
    },

    async abort(localId) {
      const session = await api.session(localId);
      if (!session) throw new HttpError(404, `session opencode:${localId} not found`);
      await api.abort(localId);
    },

    async rename(localId, name) {
      const session = await api.session(localId);
      if (!session) throw new HttpError(404, `session opencode:${localId} not found`);
      await api.rename(localId, name);
    },

    // opencode owns its runs; the bridge keeps no per-session run state to
    // report. Busy surfaces through the stream's derived working flag
    // instead.
    busyLocalIds() {
      return [];
    },

    async resolveStreamDriver(localId) {
      const session = await api.session(localId);
      if (!session) return null;

      return (handle) => {
        const driver = {
          closed: false,
          bus: null,
          refreshTimer: null,
          reconnectTimer: null,
          refreshing: false,
          refreshQueued: false,
          delivered: false,
          working: null,

          overlayEntry() {
            return null; // opencode streams inside its message list; no overlay
          },

          resolvesOverlay() {
            return false;
          },

          // Coalesced refetch: many events can land inside one interval; one
          // in-flight fetch at a time, with one queued rerun so the last
          // event is never lost.
          scheduleRefresh() {
            if (this.closed || this.refreshTimer) return;
            this.refreshTimer = setTimeout(() => {
              this.refreshTimer = null;
              this.refresh();
            }, REFRESH_INTERVAL_MS);
          },

          async refresh() {
            if (this.closed) return;
            if (this.refreshing) {
              this.refreshQueued = true;
              return;
            }
            this.refreshing = true;
            try {
              const messages = await fetchMappedMessages(localId);
              if (this.closed) return;
              handle.deliverTranscript(messages, { snapshot: !this.delivered });
              this.delivered = true;
              const working = derivedWorking(messages);
              if (working !== this.working) {
                this.working = working;
                handle.setWorking(working);
              }
            } catch {
              // opencode serve hiccupped; the next event or reconnect retries
            } finally {
              this.refreshing = false;
              if (this.refreshQueued) {
                this.refreshQueued = false;
                this.scheduleRefresh();
              }
            }
          },

          connect() {
            if (this.closed) return;
            this.bus = api.subscribeEvents(
              (event) => {
                if (event?.properties?.sessionID !== localId) return;
                this.scheduleRefresh();
              },
              () => {
                if (this.closed) return;
                this.reconnectTimer = setTimeout(() => {
                  this.reconnectTimer = null;
                  this.connect();
                  this.scheduleRefresh(); // converge whatever happened while dark
                }, RECONNECT_DELAY_MS);
                this.reconnectTimer.unref?.();
              }
            );
          },

          close() {
            this.closed = true;
            if (this.refreshTimer) clearTimeout(this.refreshTimer);
            if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
            this.bus?.close();
          },
        };

        driver.connect();
        driver.refresh(); // the initial snapshot; sets working from it
        return driver;
      };
    },

    shutdown() {},
  };
}
