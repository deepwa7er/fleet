// The dense symbol table — DG-001 §5's hero component. Used by the module
// view, search results, and the caller/callee lists on the symbol page.

import type { LinkedSymbol, SymbolSummary } from "../api";
import { KindTag } from "./KindTag";
import { Signature } from "./Signature";

function shortLocation(s: SymbolSummary): string {
  if (!s.file) return s.is_external ? "external" : "—";
  const file = s.file.split("/").slice(-2).join("/");
  return s.start_line !== null ? `${file}:${s.start_line + 1}` : file;
}

export function SymbolTable({
  symbols,
  onOpen,
  context,
  truncated,
}: {
  symbols: (SymbolSummary | LinkedSymbol)[];
  onOpen: (id: number) => void;
  /** Show the crate::module path column (off inside a single module). */
  context?: boolean;
  truncated?: boolean;
}) {
  if (symbols.length === 0) {
    return <p className="empty">none</p>;
  }
  const hasCount = symbols.some((s) => "count" in s);
  return (
    <>
      <table className="symbols">
        <thead>
          <tr>
            <th className="col-kind">kind</th>
            <th>name</th>
            {context && <th>where</th>}
            <th>signature</th>
            <th className="col-loc">source</th>
            {hasCount && <th className="num col-count">refs</th>}
          </tr>
        </thead>
        <tbody>
          {symbols.map((s) => (
            <tr key={`${s.id}-${"edge_kind" in s ? s.edge_kind : ""}`}>
              <td className="col-kind">
                <KindTag kind={s.kind} />
              </td>
              <td>
                <button className="sym-link" onClick={() => onOpen(s.id)} title={s.display}>
                  {s.container && <span className="sym-container">{s.container}::</span>}
                  {s.name}
                </button>
                {s.trait_name && <span className="sym-trait"> as {s.trait_name}</span>}
              </td>
              {context && (
                <td className="sym-where">
                  {s.module_path ? `${s.crate_name}::${s.module_path}` : s.crate_name}
                </td>
              )}
              <td className="sym-sig">{s.signature ? <Signature text={s.signature} /> : "—"}</td>
              <td className="col-loc sym-loc" title={s.file ?? undefined}>
                {shortLocation(s)}
              </td>
              {hasCount && (
                <td className="num col-count">
                  {"count" in s ? (s.edge_kind === "use" ? `${s.count} use` : s.count) : ""}
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
      {truncated && <p className="truncated">list truncated at 200 rows</p>}
    </>
  );
}
