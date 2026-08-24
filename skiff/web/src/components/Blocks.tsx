import type { Block, Inline, Token, TokenClass } from "../types"

/**
 * Typed blocks → components (DW-004 §8).
 *
 * Nothing here parses. The server sent structure and pre-resolved highlight
 * tokens; this file's only job is to choose an element per block kind. There
 * is deliberately no `dangerouslySetInnerHTML` anywhere — model output reaches
 * the page as text nodes, so a reply containing a tag renders as characters.
 */
export function Blocks({ blocks }: { blocks: Block[] }) {
  return blocks.map((block, i) => <BlockView key={i} block={block} />)
}

function BlockView({ block }: { block: Block }) {
  switch (block.kind) {
    case "paragraph":
      return <p><Inlines inlines={block.inlines} /></p>
    case "heading":
      return <Heading level={block.level} inlines={block.inlines} />
    case "code":
      return <CodeBlock lang={block.lang} tokens={block.tokens} />
    case "list":
      return <ListView block={block} />
    case "quote":
      // DW-001 rule 1: no border. The quote is set apart by indent and muted
      // ink, not by a rule down its left edge.
      return (
        <blockquote className="ml-4 flex flex-col gap-3 text-muted">
          <Blocks blocks={block.blocks} />
        </blockquote>
      )
    case "table":
      return <TableView head={block.head} rows={block.rows} />
    case "rule":
      // The one place a line is content rather than decoration: the author
      // wrote a break, so it is not the divider rule 1 forbids.
      return <hr className="border-0 border-t border-muted/30" />
  }
}

function Heading({ level, inlines }: { level: number; inlines: Inline[] }) {
  // DW-001 §3: headings are the serif voice, and tracking tightens as size
  // grows. A message's headings start a step down from the page's own.
  const Tag = (`h${Math.min(level + 2, 6)}`) as "h3" | "h4" | "h5" | "h6"
  const size = level <= 1 ? "text-lg tracking-tight" : "text-base"
  return (
    <Tag className={`font-heading font-semibold ${size}`}>
      <Inlines inlines={inlines} />
    </Tag>
  )
}

function ListView({ block }: { block: Extract<Block, { kind: "list" }> }) {
  const items = block.items.map((item, i) => (
    <li key={i} className="flex flex-col gap-2">
      <Blocks blocks={item} />
    </li>
  ))
  return block.ordered ? (
    <ol start={block.start ?? undefined} className="ml-5 flex list-decimal flex-col gap-2">
      {items}
    </ol>
  ) : (
    <ul className="ml-5 flex list-disc flex-col gap-2">{items}</ul>
  )
}

function TableView({ head, rows }: { head: Inline[][]; rows: Inline[][][] }) {
  // DW-001 §4: anything wide scrolls inside its own box, so the page itself
  // never scrolls sideways.
  return (
    <div className="overflow-x-auto">
      <table className="w-full tabular-nums">
        {head.length > 0 && (
          <thead>
            <tr>
              {head.map((cell, i) => (
                <th key={i} className="instrumentation py-1 pr-4 text-left">
                  <Inlines inlines={cell} />
                </th>
              ))}
            </tr>
          </thead>
        )}
        <tbody>
          {rows.map((row, i) => (
            <tr key={i}>
              {row.map((cell, j) => (
                <td key={j} className="py-1 pr-4 align-top">
                  <Inlines inlines={cell} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/** DW-001 §2: `--fill` is how a surface becomes distinct without a border. */
function CodeBlock({ lang, tokens }: { lang: string | null; tokens: Token[] }) {
  return (
    <div className="flex flex-col bg-fill">
      {lang && <span className="instrumentation px-3 pt-2">{lang}</span>}
      <pre className="overflow-x-auto px-3 py-2 font-mono text-sm">
        <code>
          {tokens.map((token, i) => (
            <TokenView key={i} token={token} />
          ))}
        </code>
      </pre>
    </div>
  )
}

/**
 * DW-001 §8: highlighting reuses the app's status palette rather than
 * introducing a second one — and anything that is not one of the five roles
 * stays the block's own ink, unspanned.
 */
const TOKEN_CLASS: Record<Exclude<TokenClass, "plain">, string> = {
  keyword: "text-accent",
  str: "text-good",
  comment: "text-muted",
  deleted: "text-danger",
  inserted: "text-good",
}

function TokenView({ token }: { token: Token }) {
  if (token.class === "plain") return token.text
  return <span className={TOKEN_CLASS[token.class]}>{token.text}</span>
}

function Inlines({ inlines }: { inlines: Inline[] }) {
  return inlines.map((inline, i) => <InlineView key={i} inline={inline} />)
}

function InlineView({ inline }: { inline: Inline }) {
  switch (inline.kind) {
    case "text":
      return inline.text
    case "code":
      return <code className="bg-fill px-1 font-mono text-[0.9em]">{inline.text}</code>
    case "emph":
      return <em><Inlines inlines={inline.inlines} /></em>
    case "strong":
      return <strong className="font-semibold"><Inlines inlines={inline.inlines} /></strong>
    case "strike":
      return <s className="text-muted"><Inlines inlines={inline.inlines} /></s>
    case "link":
      // DW-001 rule 4: if it's blue, you can click it.
      return (
        <a href={inline.href} className="text-accent underline underline-offset-2">
          <Inlines inlines={inline.inlines} />
        </a>
      )
    case "break":
      return <br />
  }
}
