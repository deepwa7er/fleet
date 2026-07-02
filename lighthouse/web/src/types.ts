/** Reference data merged from the docs site's fleet.json (see backend
 * src/fleet.rs). Present only for units the fleet describes. */
export interface FleetRef {
  /** One-line service description (the docs' authoritative summary). */
  summary: string;
  /** Public URL, when routed — the card's "open" link. */
  url?: string;
  /** Loopback port the service listens on. */
  port?: number;
  /** Git remote. */
  repo?: string;
  /** Deep link to the service's entry on the docs site. */
  docs_url?: string;
}

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
  /** Reference data from the fleet docs, when this unit is described there. */
  fleet?: FleetRef;
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
  deployed: {
    short: string;
    /** GitHub link to this commit, when the repo is on GitHub. */
    commit_url: string | null;
    dirty: boolean;
    deployed_at: number;
  } | null;
  local: {
    branch: string | null;
    head_short: string | null;
    dirty: boolean;
    undeployed_commits: number | null;
  } | null;
  /** GitHub compare link for the undeployed commits, when there are any. */
  changes_url: string | null;
}

export interface DeployHistoryEntry {
  /** Deploy id; present means a saved transcript can be opened. `null` for
   *  pre-v2 deploys, which were never captured. */
  id: string | null;
  sha: string;
  short: string;
  /** GitHub link to this commit, when the repo is on GitHub. */
  commit_url: string | null;
  branch: string | null;
  dirty: boolean;
  result: "deployed" | "rolled_back";
  /** Unix epoch seconds. */
  at: number;
}

/** One commit a deploy shipped — the range from the previously-deployed sha to
 *  the one this deploy shipped. */
export interface ChangelogCommit {
  short: string;
  /** First line of the commit message. */
  subject: string;
  /** GitHub link to this commit, when the repo is on GitHub. */
  commit_url: string | null;
  /** Unix epoch seconds. */
  at: number;
}

/** One cell of the health timeline. `gap` means no samples were collected then
 *  (e.g. lighthouse itself was down). `unreachable` means systemd was active but
 *  the service couldn't be reached through breakwater. */
export type HealthStatus = "up" | "unreachable" | "down" | "gap";

export interface HealthBucket {
  /** Unix epoch seconds — the cell's start. */
  at: number;
  status: HealthStatus;
  /** Peak memory in the cell, for the sparkline. */
  memory_bytes: number | null;
}

export interface CurrentProbe {
  ok: boolean;
  status: number | null;
  ms: number | null;
  /** Unix epoch seconds when the current reachable/unreachable state began. */
  since: number;
}

export interface HealthSummary {
  sample_count: number;
  /** Percent of samples whose systemd state was `active` (0–100). */
  systemd_uptime_pct: number;
  /** Whether this service is probed (it has a public URL). */
  probed: boolean;
  /** Percent of probed samples reachable through breakwater, or null. */
  probe_uptime_pct: number | null;
  /** Current reachability, or null when not probed. */
  current: CurrentProbe | null;
  memory_current: number | null;
  memory_peak: number | null;
}

export interface HealthHistory {
  window_secs: number;
  /** Server's current time (epoch seconds), to anchor the timeline. */
  now: number;
  interval_secs: number;
  summary: HealthSummary;
  buckets: HealthBucket[];
}
