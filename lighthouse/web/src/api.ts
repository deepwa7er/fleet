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
