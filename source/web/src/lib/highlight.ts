// Syntax highlighting via Shiki. We keep one highlighter instance and load
// languages/themes on demand, so the first view of a given language pays the
// grammar-load cost once and subsequent views are instant.

import {
  createHighlighter,
  type Highlighter,
  type BundledLanguage,
  type BundledTheme,
} from "shiki";
import type { Theme } from "./theme";

// Muted, low-contrast themes that sit well with the fleet's terminal aesthetic.
const THEMES: Record<Theme, BundledTheme> = {
  dark: "vitesse-dark",
  light: "vitesse-light",
};

let highlighterPromise: Promise<Highlighter> | null = null;

function getHighlighter(): Promise<Highlighter> {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({
      themes: [THEMES.dark, THEMES.light],
      langs: [],
    });
  }
  return highlighterPromise;
}

// Languages Shiki actually bundles a grammar for. An unknown id (our fallback
// `text`, or an odd extension) is rendered as plain text rather than throwing.
async function ensureLang(hl: Highlighter, lang: string): Promise<string> {
  if (hl.getLoadedLanguages().includes(lang)) return lang;
  try {
    await hl.loadLanguage(lang as BundledLanguage);
    return lang;
  } catch {
    return "txt";
  }
}

// Highlight `code` to HTML. Shiki wraps each line in `<span class="line">`, which
// the stylesheet numbers via a CSS counter. Returns a `{ html }` the caller sets
// with dangerouslySetInnerHTML — the input is our own source text, rendered by
// Shiki's tokenizer (no raw HTML passthrough).
export async function highlight(
  code: string,
  lang: string,
  theme: Theme,
): Promise<string> {
  const hl = await getHighlighter();
  const resolved = await ensureLang(hl, lang);
  return hl.codeToHtml(code, {
    lang: resolved,
    theme: THEMES[theme],
  });
}
