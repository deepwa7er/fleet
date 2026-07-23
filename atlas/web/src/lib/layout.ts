// Layered layout for the trace DAG. Depth (from the server's BFS) fixes each
// node's column; within a column, rows are ordered by the barycenter of their
// neighbors in the adjacent column, swept a few times in both directions to
// cut crossings. Deterministic, no dependencies — a few dozen lines beat a
// generic layout library for graphs this shaped.

import type { TraceEdge, TraceNode } from "../api";

export interface LaidOutNode {
  node: TraceNode;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface LaidOutEdge {
  edge: TraceEdge;
  /** Bezier path between node borders. */
  path: string;
  /** An edge landing at the same or an earlier column (a cycle back edge). */
  back: boolean;
}

export interface Layout {
  nodes: LaidOutNode[];
  edges: LaidOutEdge[];
  width: number;
  height: number;
  /** x offset of each depth column, for the depth ruler. */
  columns: { depth: number; x: number; width: number }[];
}

export const NODE_HEIGHT = 34;
const ROW_GAP = 10;
const COLUMN_GAP = 56;
const PADDING = 16;
/** Approximate mono character width at 12px — labels size the boxes. */
const CH = 7.3;
const NODE_PAD_X = 8;

export function nodeLabel(n: TraceNode): { title: string; sub: string } {
  const title = n.container ? `${n.container}::${n.name}` : n.name;
  const sub = n.module_path ? `${n.crate_name}::${n.module_path}` : n.crate_name;
  return { title, sub };
}

function nodeWidth(n: TraceNode): number {
  const { title, sub } = nodeLabel(n);
  return Math.max(title.length * CH, sub.length * CH * 0.85) + NODE_PAD_X * 2;
}

export function layout(nodes: TraceNode[], edges: TraceEdge[]): Layout {
  if (nodes.length === 0) {
    return { nodes: [], edges: [], width: 0, height: 0, columns: [] };
  }

  const depths = [...new Set(nodes.map((n) => n.depth))].sort((a, b) => a - b);
  const byDepth = new Map<number, TraceNode[]>(depths.map((d) => [d, []]));
  for (const n of nodes) byDepth.get(n.depth)!.push(n);

  // Neighbor ids per node, split by direction relative to the column order.
  const forward = new Map<number, number[]>();
  const backward = new Map<number, number[]>();
  const depthOf = new Map(nodes.map((n) => [n.id, n.depth]));
  for (const e of edges) {
    const df = depthOf.get(e.from);
    const dt = depthOf.get(e.to);
    if (df === undefined || dt === undefined) continue;
    if (dt > df) {
      (forward.get(e.from) ?? forward.set(e.from, []).get(e.from)!).push(e.to);
      (backward.get(e.to) ?? backward.set(e.to, []).get(e.to)!).push(e.from);
    }
  }

  // Barycenter ordering: initial order is the server's (stable), then sweep.
  const rank = new Map<number, number>();
  const reRank = (column: TraceNode[]) => column.forEach((n, i) => rank.set(n.id, i));
  for (const d of depths) reRank(byDepth.get(d)!);

  const sortBy = (column: TraceNode[], neighbors: Map<number, number[]>) => {
    const keys = new Map<number, number>();
    for (const n of column) {
      const ns = neighbors.get(n.id) ?? [];
      const known = ns.filter((id) => rank.has(id));
      keys.set(
        n.id,
        known.length
          ? known.reduce((s, id) => s + rank.get(id)!, 0) / known.length
          : rank.get(n.id)!,
      );
    }
    column.sort((a, b) => keys.get(a.id)! - keys.get(b.id)! || rank.get(a.id)! - rank.get(b.id)!);
    reRank(column);
  };

  for (let sweep = 0; sweep < 3; sweep++) {
    for (let i = 1; i < depths.length; i++) sortBy(byDepth.get(depths[i])!, backward);
    for (let i = depths.length - 2; i >= 0; i--) sortBy(byDepth.get(depths[i])!, forward);
  }

  // Positions: columns left to right, rows top-aligned.
  const columns: Layout["columns"] = [];
  const placed = new Map<number, LaidOutNode>();
  let x = PADDING;
  let maxY = 0;
  for (const d of depths) {
    const column = byDepth.get(d)!;
    const width = Math.max(...column.map(nodeWidth));
    columns.push({ depth: d, x, width });
    let y = PADDING + 20; // below the depth ruler
    for (const n of column) {
      placed.set(n.id, { node: n, x, y, width: nodeWidth(n), height: NODE_HEIGHT });
      y += NODE_HEIGHT + ROW_GAP;
    }
    maxY = Math.max(maxY, y);
    x += width + COLUMN_GAP;
  }

  const laidEdges: LaidOutEdge[] = [];
  for (const e of edges) {
    const from = placed.get(e.from);
    const to = placed.get(e.to);
    if (!from || !to) continue;
    const back = to.node.depth <= from.node.depth;
    laidEdges.push({ edge: e, back, path: back ? backEdgePath(from, to) : edgePath(from, to) });
  }

  return {
    nodes: [...placed.values()],
    edges: laidEdges,
    width: x - COLUMN_GAP + PADDING,
    height: maxY + PADDING,
    columns,
  };
}

/** Right border of `from` to left border of `to`, eased horizontally. */
function edgePath(from: LaidOutNode, to: LaidOutNode): string {
  const x1 = from.x + from.width;
  const y1 = from.y + from.height / 2;
  const x2 = to.x;
  const y2 = to.y + to.height / 2;
  const dx = Math.max((x2 - x1) / 2, 16);
  return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
}

/** A cycle edge: loop out of the right side and back to the target's right. */
function backEdgePath(from: LaidOutNode, to: LaidOutNode): string {
  const x1 = from.x + from.width;
  const y1 = from.y + from.height / 2;
  const x2 = to.x + to.width;
  const y2 = to.y + to.height / 2;
  const bulge = 28;
  return `M ${x1} ${y1} C ${x1 + bulge} ${y1}, ${x2 + bulge} ${y2}, ${x2} ${y2}`;
}
