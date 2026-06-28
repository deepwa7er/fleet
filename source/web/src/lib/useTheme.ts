// React hook that tracks the current fleet theme as it's flipped on
// <html data-theme> (by lib/theme.ts polling tide), so components that depend on
// it — the Shiki-highlighted file view — re-render on a live theme change.

import { useEffect, useState } from "react";
import { currentTheme, type Theme } from "./theme";

export function useTheme(): Theme {
  const [theme, setTheme] = useState<Theme>(currentTheme);
  useEffect(() => {
    const el = document.documentElement;
    const observer = new MutationObserver(() => setTheme(currentTheme()));
    observer.observe(el, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);
  return theme;
}
