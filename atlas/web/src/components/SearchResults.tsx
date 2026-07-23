// Search results for one project, densest form: the shared symbol table.

import { useEffect, useRef, useState } from "react";
import { searchSymbols, type SymbolSummary } from "../api";
import { SymbolTable } from "./SymbolTable";

export function SearchResults({
  project,
  query,
  onOpen,
}: {
  project: string;
  query: string;
  onOpen: (id: number) => void;
}) {
  const [results, setResults] = useState<SymbolSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const req = useRef(0);

  useEffect(() => {
    const r = ++req.current;
    setResults(null);
    setError(null);
    searchSymbols(project, query)
      .then((s) => {
        if (req.current === r) setResults(s);
      })
      .catch((e) => {
        if (req.current === r) setError(String(e));
      });
  }, [project, query]);

  if (error) return <p className="error">{error}</p>;
  if (!results) return <p className="empty">searching…</p>;
  return (
    <section className="pane-pad">
      <h2>
        “{query}” <span className="count">{results.length} hits</span>
      </h2>
      <SymbolTable symbols={results} onOpen={onOpen} context />
    </section>
  );
}
