// Signature rendering under DG-002 §5, the lexical subset: keywords are the
// language's skeleton (steel, bold), punctuation its plumbing (faint),
// capitalized names are shapes (lcd-ink), literals are data. Full semantic
// coloring needs resolution; a one-line signature doesn't warrant it.

const KEYWORDS = new Set([
  "pub", "fn", "struct", "enum", "trait", "impl", "type", "const", "static",
  "async", "unsafe", "extern", "where", "dyn", "mut", "ref", "for", "in",
  "crate", "super", "self", "Self", "use", "mod", "let",
]);

type Piece = { text: string; cls: string | null };

function classify(token: string): string | null {
  if (KEYWORDS.has(token)) return "sig-kw";
  if (/^[0-9]/.test(token)) return "sig-lit";
  if (/^'[a-z_]/.test(token)) return "sig-life";
  if (/^[A-Z]/.test(token)) return "sig-type";
  if (/^[a-z_]/.test(token)) return null; // value names: plain ink
  return "sig-punct";
}

export function tokenize(signature: string): Piece[] {
  const pieces: Piece[] = [];
  const re = /('?[A-Za-z_][A-Za-z0-9_]*|[0-9][A-Za-z0-9_.]*|\s+|.)/g;
  for (const m of signature.matchAll(re)) {
    const text = m[0];
    pieces.push({ text, cls: /^\s+$/.test(text) ? null : classify(text) });
  }
  return pieces;
}

export function Signature({ text }: { text: string }) {
  return (
    <code className="sig">
      {tokenize(text).map((p, i) =>
        p.cls ? (
          <span key={i} className={p.cls}>
            {p.text}
          </span>
        ) : (
          p.text
        ),
      )}
    </code>
  );
}
