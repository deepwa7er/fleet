import { useEffect, useRef, useState } from "react";
import type { FileContents } from "../api";
import { highlight } from "../lib/highlight";
import { useTheme } from "../lib/useTheme";

interface Target {
  repo: string;
  path: string;
  line?: number;
}

interface Props {
  target: Target | null;
  file: FileContents | null;
  error: string | null;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export function FileView({ target, file, error }: Props) {
  const theme = useTheme();
  const [html, setHtml] = useState<string | null>(null);
  const [highlightError, setHighlightError] = useState(false);
  const codeRef = useRef<HTMLDivElement>(null);

  // Re-highlight whenever the content or the theme changes.
  useEffect(() => {
    let live = true;
    setHtml(null);
    setHighlightError(false);
    if (file?.content == null) return;
    highlight(file.content, file.language, theme)
      .then((out) => {
        if (live) setHtml(out);
      })
      .catch(() => {
        if (live) setHighlightError(true);
      });
    return () => {
      live = false;
    };
  }, [file?.content, file?.language, theme]);

  // After render, scroll to and mark the target line (from a search hit).
  useEffect(() => {
    if (!html || !target?.line || !codeRef.current) return;
    const lines = codeRef.current.querySelectorAll<HTMLElement>(".line");
    const el = lines[target.line - 1];
    if (el) {
      el.classList.add("line-hit");
      el.scrollIntoView({ block: "center" });
    }
  }, [html, target?.line]);

  if (error) {
    return (
      <div className="fileview">
        <div className="error pad">{error}</div>
      </div>
    );
  }
  if (!target) return null;
  if (!file) {
    return (
      <div className="fileview">
        <FileHeader repo={target.repo} path={target.path} />
        <div className="muted pad">loading…</div>
      </div>
    );
  }

  return (
    <div className="fileview">
      <FileHeader
        repo={file.repo}
        path={file.path}
        meta={`${file.language} · ${formatBytes(file.bytes)}`}
      />
      {file.binary && <div className="notice pad">Binary file — not shown.</div>}
      {file.too_large && (
        <div className="notice pad">
          File is {formatBytes(file.bytes)} — too large to display.
        </div>
      )}
      {file.content != null && (
        <div className="code-wrap">
          {html ? (
            <div
              className="code"
              ref={codeRef}
              // Input is our own source text rendered by Shiki's tokenizer; no
              // untrusted HTML is passed through.
              dangerouslySetInnerHTML={{ __html: html }}
            />
          ) : highlightError ? (
            // Fallback: show the raw source if highlighting somehow failed.
            <pre className="code raw">
              <code>{file.content}</code>
            </pre>
          ) : (
            <div className="muted pad">highlighting…</div>
          )}
        </div>
      )}
    </div>
  );
}

function FileHeader({
  repo,
  path,
  meta,
}: {
  repo: string;
  path: string;
  meta?: string;
}) {
  return (
    <div className="fileheader">
      <span className="loc">
        <span className="repo">{repo}</span>
        <span className="sep">/</span>
        <span className="path">{path}</span>
      </span>
      {meta && <span className="meta">{meta}</span>}
    </div>
  );
}
