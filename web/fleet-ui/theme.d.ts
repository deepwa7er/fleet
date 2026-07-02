export type Theme = "dark" | "light";

/** Start syncing the document theme from tide (no-op off the fleet). */
export function startTheme(): void;

/** Current theme as set on `<html data-theme>`, defaulting to dark. */
export function currentTheme(): Theme;
