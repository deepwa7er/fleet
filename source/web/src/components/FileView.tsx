import { useEffect, useRef, useState, type ReactNode } from "react";
import { saveFile, type FileContents } from "../api";
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
  // Called with the re-read file after a successful save, so the parent's state
  // reflects the committed content.
  onSaved: (file: FileContents) => void;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export function FileView({ target, file, error, onSaved }: Props) {
  const theme = useTheme();
  const [html, setHtml] = useState<string | null>(null);
  const [highlightError, setHighlightError] = useState(false);
  const codeRef = useRef<HTMLDivElement>(null);

  // Edit state. `editing` holds the draft + message; null when just viewing.
  const [editing, setEditing] = useState<{ draft: string; message: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<{ kind: "ok" | "warn" | "err"; text: string } | null>(null);

  const fileKey = file ? `${file.repo}/${file.path}` : null;

  // Leaving a file (or opening another) cancels any in-progress edit and clears
  // transient status.
  useEffect(() => {
    setEditing(null);
    setStatus(null);
  }, [fileKey]);

  // Re-highlight whenever the content or theme changes (view mode only).
  useEffect(() => {
    let live = true;
    setHtml(null);
    setHighlightError(false);
    if (editing || file?.content == null) return;
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
  }, [file?.content, file?.language, theme, editing]);

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

  const canEdit = file.content != null && !file.binary && !file.too_large;

  const startEdit = () => {
    setStatus(null);
    setEditing({ draft: file.content ?? "", message: `Update ${file.path}` });
  };

  const onSave = async () => {
    if (!editing) return;
    setSaving(true);
    setStatus(null);
    try {
      const res = await saveFile(file.repo, file.path, editing.draft, editing.message);
      onSaved(res.file);
      setEditing(null);
      if (!res.committed) {
        setStatus({ kind: "ok", text: "no changes" });
      } else if (res.warning) {
        setStatus({ kind: "warn", text: res.warning });
      } else {
        setStatus({
          kind: "ok",
          text: `committed ${res.commit}${res.pushed ? " · pushed" : ""}`,
        });
      }
    } catch (e) {
      setStatus({ kind: "err", text: String(e instanceof Error ? e.message : e) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fileview">
      <FileHeader
        repo={file.repo}
        path={file.path}
        meta={`${file.language} · ${formatBytes(file.bytes)}`}
        action={
          editing ? (
            <span className="edit-actions">
              <button className="btn" onClick={() => setEditing(null)} disabled={saving}>
                cancel
              </button>
              <button className="btn primary" onClick={onSave} disabled={saving}>
                {saving ? "saving…" : "save"}
              </button>
            </span>
          ) : (
            canEdit && (
              <button className="btn" onClick={startEdit}>
                edit
              </button>
            )
          )
        }
      />

      {status && <div className={`status ${status.kind}`}>{status.text}</div>}

      {editing ? (
        <div className="editor">
          <input
            className="commit-msg"
            value={editing.message}
            onChange={(e) => setEditing({ ...editing, message: e.target.value })}
            placeholder="commit message"
            spellCheck={false}
          />
          <textarea
            className="code-edit"
            value={editing.draft}
            spellCheck={false}
            autoFocus
            onChange={(e) => setEditing({ ...editing, draft: e.target.value })}
            onKeyDown={(e) => {
              // Insert two spaces on Tab instead of moving focus.
              if (e.key === "Tab") {
                e.preventDefault();
                const ta = e.currentTarget;
                const { selectionStart: s, selectionEnd: end } = ta;
                const next = editing.draft.slice(0, s) + "  " + editing.draft.slice(end);
                setEditing({ ...editing, draft: next });
                requestAnimationFrame(() => {
                  ta.selectionStart = ta.selectionEnd = s + 2;
                });
              }
            }}
          />
        </div>
      ) : (
        <>
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
                <pre className="code raw">
                  <code>{file.content}</code>
                </pre>
              ) : (
                <div className="muted pad">highlighting…</div>
              )}
            </div>
          )}
        </>
      )}
    </div>
  );
}

function FileHeader({
  repo,
  path,
  meta,
  action,
}: {
  repo: string;
  path: string;
  meta?: string;
  action?: ReactNode;
}) {
  return (
    <div className="fileheader">
      <span className="loc">
        <span className="repo">{repo}</span>
        <span className="sep">/</span>
        <span className="path">{path}</span>
      </span>
      <span className="fileheader-right">
        {meta && <span className="meta">{meta}</span>}
        {action}
      </span>
    </div>
  );
}
