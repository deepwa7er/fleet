import type { LogEntry, ServiceStatus, Source } from "./types.ts";

const BASE = "/api";

async function getJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`request to ${url} failed: ${response.status}`);
  }
  return response.json() as Promise<T>;
}

/** Base path for one service's endpoints. */
function servicePath(source: Source, id: string): string {
  return `${BASE}/services/${source}/${encodeURIComponent(id)}`;
}

export function fetchServices(): Promise<ServiceStatus[]> {
  return getJson<ServiceStatus[]>(`${BASE}/services`);
}

export function fetchLogs(
  source: Source,
  id: string,
  lines = 200,
): Promise<LogEntry[]> {
  return getJson<LogEntry[]>(`${servicePath(source, id)}/logs?lines=${lines}`);
}

/** URL for the Server-Sent Events live log stream of a service. */
export function logStreamUrl(source: Source, id: string): string {
  return `${servicePath(source, id)}/logs/stream`;
}

export type ServiceAction = "start" | "stop" | "restart";

/** Start, stop, or restart a service; resolves to its post-action status. */
export async function controlService(
  source: Source,
  id: string,
  action: ServiceAction,
): Promise<ServiceStatus> {
  const response = await fetch(`${servicePath(source, id)}/control/${action}`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`failed to ${action} ${id}: ${response.status}`);
  }
  return response.json() as Promise<ServiceStatus>;
}
