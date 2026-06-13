import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** Merge Tailwind class names, resolving conflicts (matches the blog project). */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

/** Tailwind classes for a systemd ActiveState's status dot and text. */
export function statusColor(state: string): { dot: string; text: string } {
  switch (state) {
    case "active":
      return { dot: "bg-emerald-500", text: "text-emerald-400" };
    case "failed":
      return { dot: "bg-red-500", text: "text-red-400" };
    case "activating":
    case "deactivating":
    case "reloading":
      return { dot: "bg-amber-500", text: "text-amber-400" };
    default:
      return { dot: "bg-slate-500", text: "text-slate-400" };
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

/** Format a journald microsecond timestamp as a local time string. */
export function formatTimestamp(timestampUs: number): string {
  const date = new Date(timestampUs / 1000);
  return date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
