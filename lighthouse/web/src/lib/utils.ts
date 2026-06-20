import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

import type { DeployVerdict } from "../types.ts";

/** Merge Tailwind class names, resolving conflicts (matches the blog project). */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

/** Tailwind classes for a systemd ActiveState's status dot and text (DG-001
 * flat functional status ramp; the color is the datum). */
export function statusColor(state: string): { dot: string; text: string } {
  switch (state) {
    case "active":
      return { dot: "bg-active", text: "text-active" };
    case "failed":
      return { dot: "bg-failed", text: "text-failed" };
    case "activating":
    case "deactivating":
    case "reloading":
      return { dot: "bg-warn", text: "text-warn" };
    default:
      return { dot: "bg-idle", text: "text-ink-muted" };
  }
}

/** Format a byte count as a human-readable size (e.g. "12.3 MB"). */
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const exponent = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1,
  );
  const value = bytes / 1024 ** exponent;
  return `${value.toFixed(exponent === 0 ? 0 : 1)} ${units[exponent]}`;
}

/** Tailwind classes + whether a deploy verdict warrants attention (a sidebar
 * badge and an emphasized Deploy button). `current`/`unknown` are quiet. */
export function deployVerdictStyle(verdict: DeployVerdict): {
  dot: string;
  text: string;
  attention: boolean;
} {
  switch (verdict) {
    case "current":
      return { dot: "bg-active", text: "text-active", attention: false };
    case "stale":
      return { dot: "bg-accent", text: "text-accent", attention: true };
    case "dirty":
    case "diverged":
      return { dot: "bg-warn", text: "text-warn", attention: true };
    default: // unknown
      return { dot: "bg-idle", text: "text-ink-muted", attention: false };
  }
}

/** Short human label for a deploy verdict; `undeployed` fills the stale count. */
export function deployVerdictLabel(
  verdict: DeployVerdict,
  undeployed: number | null,
): string {
  switch (verdict) {
    case "current":
      return "up to date";
    case "dirty":
      return "uncommitted changes";
    case "stale":
      return undeployed != null
        ? `${undeployed} commit${undeployed === 1 ? "" : "s"} to deploy`
        : "behind local";
    case "diverged":
      return "diverged from deployed";
    default:
      return "version unknown";
  }
}

/** Relative time from an epoch-seconds timestamp, e.g. "3m ago". */
export function formatRelative(epochSeconds: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - epochSeconds);
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/** Format a journald microsecond timestamp as a local time string. */
export function formatTimestamp(timestampUs: number): string {
  const date = new Date(timestampUs / 1000);
  return date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
