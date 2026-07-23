// One symbol: signature, docs, provenance, trait linkage, and the caller /
// callee tables. The trace buttons hand off to the trace view.

import { useEffect, useRef, useState } from "react";
import { fetchSymbol, type SymbolDetail } from "../api";
import { KindTag } from "./KindTag";
import { Signature } from "./Signature";
import { SymbolTable } from "./SymbolTable";

export function SymbolPage({
  id,
  onOpen,
  onTrace,
}: {
  id: number;
  onOpen: (id: number) => void;
  onTrace: (id: number, dir: "out" | "in") => void;
}) {
  const [detail, setDetail] = useState<SymbolDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const req = useRef(0);

  useEffect(() => {
    const r = ++req.current;
    setDetail(null);
    setError(null);
    fetchSymbol(id)
      .then((d) => {
        if (req.current === r) setDetail(d);
      })
      .catch((e) => {
        if (req.current === r) setError(String(e));
      });
  }, [id]);

  if (error) return <p className="error">{error}</p>;
  if (!detail) return <p className="empty">loading…</p>;

  const callable = ["function", "method", "static_method", "trait_method", "macro"].includes(
    detail.kind,
  );

  return (
    <article className="symbol-page">
      <header className="symbol-head">
        <div className="symbol-title">
          <KindTag kind={detail.kind} />
          <h1>{detail.display}</h1>
          {detail.is_external && <span className="badge-ext">external</span>}
        </div>
        {callable && (
          <div className="symbol-actions">
            <button className="btn" onClick={() => onTrace(detail.id, "in")}>
              ◂ trace callers
            </button>
            <button className="btn btn--primary" onClick={() => onTrace(detail.id, "out")}>
              trace callees ▸
            </button>
          </div>
        )}
      </header>

      {detail.signature && (
        <div className="symbol-sig">
          <Signature text={detail.signature} />
        </div>
      )}

      <dl className="symbol-meta">
        {detail.file && (
          <>
            <dt>source</dt>
            <dd>
              {detail.file}:{(detail.start_line ?? 0) + 1}
              {detail.end_line !== null &&
                detail.end_line !== detail.start_line &&
                `–${detail.end_line + 1}`}
            </dd>
          </>
        )}
        <dt>crate</dt>
        <dd>{detail.crate_name}</dd>
        {detail.module_path && (
          <>
            <dt>module</dt>
            <dd>{detail.module_path}</dd>
          </>
        )}
        {detail.trait_name && (
          <>
            <dt>trait</dt>
            <dd>
              {detail.trait_name}
              {detail.declaration && (
                <>
                  {" — "}
                  <button className="sym-link" onClick={() => onOpen(detail.declaration!.id)}>
                    declaration
                  </button>
                </>
              )}
            </dd>
          </>
        )}
      </dl>

      {detail.docs && <pre className="symbol-docs">{detail.docs}</pre>}

      {detail.implementations.length > 0 && (
        <section>
          <h2>
            implementations <span className="count">{detail.implementations.length}</span>
          </h2>
          <SymbolTable symbols={detail.implementations} onOpen={onOpen} context />
        </section>
      )}

      <section>
        <h2>
          callers <span className="count">{detail.callers.length}</span>
        </h2>
        <SymbolTable
          symbols={detail.callers}
          onOpen={onOpen}
          context
          truncated={detail.callers_truncated}
        />
      </section>

      <section>
        <h2>
          calls &amp; uses <span className="count">{detail.callees.length}</span>
        </h2>
        <SymbolTable
          symbols={detail.callees}
          onOpen={onOpen}
          context
          truncated={detail.callees_truncated}
        />
      </section>
    </article>
  );
}
