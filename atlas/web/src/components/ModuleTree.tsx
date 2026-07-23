// The sidebar: crates and their module trees, item counts right-aligned.
// Built from the flat module list; intermediate modules that hold no items of
// their own (only submodules) are synthesized so the tree is complete.

import { useMemo, useState } from "react";
import type { ModuleRow } from "../api";

interface TreeNode {
  name: string;
  path: string;
  items: number;
  children: TreeNode[];
}

interface CrateTree {
  crate: string;
  items: number;
  root: TreeNode;
}

function buildTrees(rows: ModuleRow[]): CrateTree[] {
  const crates = new Map<string, TreeNode>();
  const totals = new Map<string, number>();
  for (const row of rows) {
    const cached = crates.get(row.crate_name);
    let node: TreeNode = cached ?? { name: row.crate_name, path: "", items: 0, children: [] };
    if (!cached) crates.set(row.crate_name, node);
    totals.set(row.crate_name, (totals.get(row.crate_name) ?? 0) + row.items);
    if (row.module_path === "") {
      node.items = row.items;
      continue;
    }
    let path = "";
    for (const segment of row.module_path.split("::")) {
      path = path ? `${path}::${segment}` : segment;
      const existing = node.children.find((c) => c.name === segment);
      const child: TreeNode = existing ?? { name: segment, path, items: 0, children: [] };
      if (!existing) node.children.push(child);
      node = child;
    }
    node.items = row.items;
  }
  return [...crates.entries()]
    .map(([crate, root]) => ({ crate, root, items: totals.get(crate) ?? 0 }))
    .sort((a, b) => a.crate.localeCompare(b.crate));
}

function Branch({
  node,
  crate,
  depth,
  active,
  onOpen,
}: {
  node: TreeNode;
  crate: string;
  depth: number;
  active: string | null;
  onOpen: (crate: string, module: string) => void;
}) {
  const key = `${crate}//${node.path}`;
  const isActive = active === key;
  return (
    <>
      <button
        className={`tree-row${isActive ? " tree-row--active" : ""}`}
        style={{ paddingLeft: `${8 + depth * 14}px` }}
        onClick={() => onOpen(crate, node.path)}
        title={node.path || crate}
      >
        <span className="tree-name">{node.name}</span>
        {node.items > 0 && <span className="tree-count">{node.items}</span>}
      </button>
      {node.children
        .slice()
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((child) => (
          <Branch
            key={child.path}
            node={child}
            crate={crate}
            depth={depth + 1}
            active={active}
            onOpen={onOpen}
          />
        ))}
    </>
  );
}

export function ModuleTree({
  modules,
  active,
  onOpen,
}: {
  modules: ModuleRow[];
  /** `crate//module_path` of the open module, for highlighting. */
  active: string | null;
  onOpen: (crate: string, module: string) => void;
}) {
  const trees = useMemo(() => buildTrees(modules), [modules]);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const toggle = (crate: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(crate)) next.add(crate);
      return next;
    });

  return (
    <nav className="tree">
      {trees.map(({ crate, root, items }) => (
        <section key={crate} className="tree-crate">
          <div className="tree-crate-head">
            <button className="tree-fold" onClick={() => toggle(crate)}>
              {collapsed.has(crate) ? "+" : "−"}
            </button>
            <button className="tree-crate-name" onClick={() => onOpen(crate, "")}>
              {crate}
            </button>
            <span className="tree-count">{items}</span>
          </div>
          {!collapsed.has(crate) && (
            <div className="tree-branches">
              {root.children
                .slice()
                .sort((a, b) => a.name.localeCompare(b.name))
                .map((child) => (
                  <Branch
                    key={child.path}
                    node={child}
                    crate={crate}
                    depth={0}
                    active={active}
                    onOpen={onOpen}
                  />
                ))}
            </div>
          )}
        </section>
      ))}
    </nav>
  );
}
