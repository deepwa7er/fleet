// The trace view: a layered call-graph slice from one root, drawn as SVG.
// Columns are call depth; edges flow left to right; cycle back-edges loop
// around in amber. Every node is a link deeper into the graph.

import { useEffect, useRef, useState } from "react";
import { fetchTrace, type TraceGraph } from "../api";
import { kindRole } from "./KindTag";
import { layout, nodeLabel } from "../lib/layout";

export interface TraceParams {
  id: number;
  dir: "out" | "in";
  depth: number;
  externals: boolean;
}

export function TraceView({
  params,
  onOpen,
  onRetrace,
  onParams,
}: {
  params: TraceParams;
  onOpen: (id: number) => void;
  /** Re-root the trace on another node. */
  onRetrace: (id: number) => void;
  onParams: (params: TraceParams) => void;
}) {
  const [graph, setGraph] = useState<TraceGraph | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const req = useRef(0);

  useEffect(() => {
    const r = ++req.current;
    setGraph(null);
    setError(null);
    setSelected(null);
    fetchTrace(params.id, params.dir, params.depth, params.externals)
      .then((g) => {
        if (req.current === r) setGraph(g);
      })
      .catch((e) => {
        if (req.current === r) setError(String(e));
      });
  }, [params.id, params.dir, params.depth, params.externals]);

  const root = graph?.nodes.find((n) => n.id === graph.root);

  return (
    <div className="trace">
      <div className="trace-bar">
        <span className="trace-root" title={root?.display}>
          {params.dir === "out" ? "flow from" : "paths into"}{" "}
          <strong>{root ? root.display : `#${params.id}`}</strong>
        </span>
        <div className="trace-controls">
          <label className="opt">
            dir
            <select
              value={params.dir}
              onChange={(e) => onParams({ ...params, dir: e.target.value as "out" | "in" })}
            >
              <option value="out">callees ▸</option>
              <option value="in">◂ callers</option>
            </select>
          </label>
          <label className="opt">
            depth
            <select
              value={params.depth}
              onChange={(e) => onParams({ ...params, depth: Number(e.target.value) })}
            >
              {[1, 2, 3, 4, 5, 6].map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
          </label>
          <label className="opt" title="Include calls into std and dependencies">
            <input
              type="checkbox"
              checked={params.externals}
              onChange={(e) => onParams({ ...params, externals: e.target.checked })}
            />
            std/deps
          </label>
        </div>
        {graph && (
          <span className="trace-stats">
            {graph.nodes.length} fns · {graph.edges.length} calls
            {graph.truncated && <span className="trace-truncated"> · TRUNCATED</span>}
          </span>
        )}
      </div>

      {error && <p className="error">{error}</p>}
      {!graph && !error && <p className="empty">tracing…</p>}
      {graph && <TraceCanvas graph={graph} selected={selected} onSelect={setSelected} />}

      {selected !== null && graph && (
        <div className="trace-foot">
          <span className="trace-foot-name">
            {graph.nodes.find((n) => n.id === selected)?.display}
          </span>
          <button className="btn" onClick={() => onOpen(selected)}>
            open symbol
          </button>
          <button className="btn" onClick={() => onRetrace(selected)}>
            re-root trace
          </button>
        </div>
      )}
    </div>
  );
}

function TraceCanvas({
  graph,
  selected,
  onSelect,
}: {
  graph: TraceGraph;
  selected: number | null;
  onSelect: (id: number) => void;
}) {
  const laid = layout(graph.nodes, graph.edges);
  // Edges touching the selected node light up; with nothing selected the
  // root's own edges are the highlighted set.
  const focus = selected ?? graph.root;

  return (
    <div className="trace-scroll">
      <svg width={laid.width} height={laid.height} className="trace-svg">
        {laid.columns.map((c) => (
          <g key={c.depth}>
            <text x={c.x} y={18} className="trace-ruler">
              D{c.depth}
            </text>
            {c.depth > 0 && (
              <line
                x1={c.x - 28}
                y1={8}
                x2={c.x - 28}
                y2={laid.height}
                className="trace-rule"
              />
            )}
          </g>
        ))}
        {laid.edges.map(({ edge, path, back }) => {
          const hot = edge.from === focus || edge.to === focus;
          const cls = back ? "trace-edge trace-edge--back" : "trace-edge";
          return <path key={`${edge.from}-${edge.to}`} d={path} className={hot ? `${cls} trace-edge--hot` : cls} />;
        })}
        {laid.nodes.map(({ node, x, y, width, height }) => {
          const { title, sub } = nodeLabel(node);
          const isRoot = node.id === graph.root;
          const isSelected = node.id === selected;
          const classes = [
            "trace-node",
            `trace-node--${kindRole(node.kind)}`,
            isRoot ? "trace-node--root" : "",
            isSelected ? "trace-node--selected" : "",
            node.is_external ? "trace-node--ext" : "",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <g
              key={node.id}
              className={classes}
              transform={`translate(${x}, ${y})`}
              onClick={() => onSelect(node.id)}
            >
              <rect width={width} height={height} />
              <text x={8} y={14} className="trace-node-title">
                {title}
              </text>
              <text x={8} y={27} className="trace-node-sub">
                {sub}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
