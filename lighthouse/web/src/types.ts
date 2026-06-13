/** Which backend manages a service. */
export type Source = "systemd" | "docker";

export interface ServiceStatus {
  source: Source;
  /** Identifier within the source: a unit name or a container name. */
  id: string;
  name: string;
  /** Normalized state: active, inactive, failed, activating, … */
  active_state: string;
  /** Finer-grained state: running, dead, exited, restarting, … */
  sub_state: string;
  description: string;
  /** Human-readable "active since" timestamp, or null if not running. */
  since: string | null;
  memory_bytes: number | null;
  pid: number | null;
}

export interface LogEntry {
  /** Microseconds since the Unix epoch. */
  timestamp_us: number;
  /** syslog priority 0 (emerg) – 7 (debug). */
  priority: number;
  message: string;
}
