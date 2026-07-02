// Fleet-wide dark/light theme, from the tide service. First paint is set
// synchronously by the inline cookie script in each app's index.html; this
// fetches the source of truth and polls so the tab flips live when `b dark`/
// `b light` changes it. Best-effort: if tide is unreachable (e.g. off-tailnet),
// the cookie/default theme stands.
//
// This is THE fleet copy — every UI imports it from @fleet/ui/theme. Plain JS
// (with theme.d.ts alongside) so bundled apps and vanilla consumers share one
// file.

const POLL_MS = 5000;

// tide lives at `tide.<base_domain>` — the same base domain this app is served
// from — so derive its URL from the current host rather than hardcoding the
// fleet domain (which would otherwise have to change in every UI on a domain
// move). Returns null off the fleet (local dev on localhost or an IP), where
// there is no tide to reach and the cookie/default theme stands.
function tideThemeUrl() {
  const { protocol, hostname } = window.location;
  const firstDot = hostname.indexOf(".");
  if (protocol !== "https:" || firstDot <= 0) return null;
  const baseDomain = hostname.slice(firstDot + 1);
  return `https://tide.${baseDomain}/theme`;
}

/** @param {"dark" | "light"} theme */
function apply(theme) {
  const el = document.documentElement;
  el.dataset.theme = theme;
  el.style.colorScheme = theme;
}

/** Start syncing the document theme from tide (no-op off the fleet). */
export function startTheme() {
  const url = tideThemeUrl();
  if (url === null) return;
  const sync = async () => {
    try {
      const res = await fetch(url, { cache: "no-store" });
      if (!res.ok) return;
      const { theme } = await res.json();
      if (theme === "dark" || theme === "light") apply(theme);
    } catch {
      // keep the current (cookie/default) theme
    }
  };
  void sync();
  setInterval(() => void sync(), POLL_MS);
}

/**
 * Current theme as set on `<html data-theme>`, defaulting to dark.
 * @returns {"dark" | "light"}
 */
export function currentTheme() {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}
