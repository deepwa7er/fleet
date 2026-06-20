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

/** How a deployed service compares to the dev box's local working tree. */
export type DeployVerdict =
  | "current" // running local HEAD, clean
  | "dirty" // running local HEAD, but uncommitted local changes
  | "stale" // local is ahead of what's deployed
  | "diverged" // local moved off the deployed line
  | "unknown"; // never stamped, or the build host couldn't be reached

export interface DeployStatus {
  unit: string;
  verdict: DeployVerdict;
  deployed: { short: string; dirty: boolean; deployed_at: number } | null;
  local: {
    branch: string | null;
    head_short: string | null;
    dirty: boolean;
    undeployed_commits: number | null;
  } | null;
}
