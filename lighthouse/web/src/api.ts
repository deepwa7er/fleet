import type { LogEntry, ServiceStatus } from "./types.ts";

const BASE = "/api";

async function getJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`request to ${url} failed: ${response.status}`);
  }
  return response.json() as Promise<T>;
}

export function fetchServices(): Promise<ServiceStatus[]> {
  return getJson<ServiceStatus[]>(`${BASE}/services`);
}

export function fetchLogs(unit: string, lines = 200): Promise<LogEntry[]> {
  return getJson<LogEntry[]>(
    `${BASE}/services/${encodeURIComponent(unit)}/logs?lines=${lines}`,
  );
}

/** URL for the Server-Sent Events live log stream of a unit. */
export function logStreamUrl(unit: string): string {
  return `${BASE}/services/${encodeURIComponent(unit)}/logs/stream`;
}

export type ServiceAction = "start" | "stop" | "restart";

/** Start, stop, or restart a service; resolves to its post-action status. */
export async function controlService(
  unit: string,
  action: ServiceAction,
): Promise<ServiceStatus> {
  const response = await fetch(
    `${BASE}/services/${encodeURIComponent(unit)}/control/${action}`,
    { method: "POST" },
  );
  if (!response.ok) {
    throw new Error(`failed to ${action} ${unit}: ${response.status}`);
  }
  return response.json() as Promise<ServiceStatus>;
}
