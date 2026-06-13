export interface ServiceStatus {
  unit: string;
  name: string;
  /** systemd ActiveState: active, inactive, failed, activating, … */
  active_state: string;
  /** systemd SubState: running, dead, exited, … */
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
