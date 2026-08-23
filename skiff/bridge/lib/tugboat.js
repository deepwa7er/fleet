// bridge/lib/tugboat.js
// Thin client for the tugboat serve daemon (the fleet's deploy engine, on
// loopback). The deploy half of approve: the bridge triggers a full fleet
// deploy after a landing and records each job's terminal outcome on the
// change.
//
// Token-gated: `createTugboatClient` returns null without
// TUGBOAT_SERVE_TOKEN, and approvals then behave exactly as they did before
// this feature — the daemon's /deploy endpoint runs builds, so it is never
// called unauthenticated, and an absent token is the "feature off" switch.

// How long a /services count (the desk's "will deploy the whole fleet"
// preview) is reused before asking again — cheap, but a burst of review
// pages shouldn't hammer the daemon.
const SERVICES_TTL_MS = 60_000;

export function createTugboatClient({
  url = process.env.TUGBOAT_SERVE_URL ?? "http://127.0.0.1:7878",
  token = process.env.TUGBOAT_SERVE_TOKEN ?? null,
} = {}) {
  if (!token) return null;
  const base = url.replace(/\/+$/, "");
  let servicesCache = { at: 0, count: null };

  async function request(path, { method = "GET" } = {}) {
    let resp;
    try {
      resp = await fetch(`${base}${path}`, {
        method,
        headers: { authorization: `Bearer ${token}` },
      });
    } catch (err) {
      throw new Error(`tugboat daemon unreachable at ${base}: ${err.message}`);
    }
    if (!resp.ok) {
      throw new Error(`tugboat ${method} ${path} answered ${resp.status}`);
    }
    return resp.json();
  }

  return {
    /// Start a deploy job for every deployable service. `{jobs:[...]}` with
    /// one entry per service: `{name, job_id}` when a job started, `{name,
    /// status:"in_progress"}` when the service was already deploying.
    deployAll() {
      return request("/deploy", { method: "POST" });
    },

    /// One job's terminal outcome: `{id, outcome: null | {ok, error}}` —
    /// null while the deploy runs.
    jobStatus(jobId) {
      return request(`/jobs/${encodeURIComponent(jobId)}`);
    },

    /// How many deployable services the daemon knows, cached briefly.
    async serviceCount() {
      const now = Date.now();
      if (servicesCache.count !== null && now - servicesCache.at < SERVICES_TTL_MS) {
        return servicesCache.count;
      }
      const services = await request("/services");
      const count = Array.isArray(services) ? services.length : 0;
      servicesCache = { at: now, count };
      return count;
    },
  };
}
