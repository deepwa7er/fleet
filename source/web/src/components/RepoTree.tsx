import { useMemo, useState } from "react";
import type { Repo, Tree } from "../api";
import { buildTree, type TreeNode } from "../lib/tree";

interface Props {
  repos: Repo[];
  activeRepo: string | null;
  tree: Tree | null;
  treeError: string | null;
  openPath: string | null;
  onSelectRepo: (name: string) => void;
  onOpenFile: (repo: string, path: string) => void;
}

export function RepoTree({
  repos,
  activeRepo,
  tree,
  treeError,
  openPath,
  onSelectRepo,
  onOpenFile,
}: Props) {
  const nodes = useMemo(
    () => (tree ? buildTree(tree.files) : []),
    [tree],
  );

  return (
    <div className="repotree">
      <div className="pane-label">REPOS</div>
      <ul className="repolist">
        {repos.map((r) => (
          <li key={r.name}>
            <button
              className={r.name === activeRepo ? "repo active" : "repo"}
              onClick={() => onSelectRepo(r.name)}
            >
              {r.name}
            </button>
          </li>
        ))}
      </ul>

      {activeRepo && (
        <>
          <div className="pane-label">
            {activeRepo}
            {tree && <span className="count"> · {tree.files.length} files</span>}
          </div>
          {treeError && <div className="error">{treeError}</div>}
          {!tree && !treeError && <div className="muted">loading…</div>}
          {tree && (
            <ul className="tree">
              {nodes.map((n) => (
                <TreeItem
                  key={n.path}
                  node={n}
                  depth={0}
                  repo={activeRepo}
                  openPath={openPath}
                  onOpenFile={onOpenFile}
                />
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

interface ItemProps {
  node: TreeNode;
  depth: number;
  repo: string;
  openPath: string | null;
  onOpenFile: (repo: string, path: string) => void;
}

function TreeItem({ node, depth, repo, openPath, onOpenFile }: ItemProps) {
  // Top-level directories start open; deeper ones collapsed, to keep the initial
  // tree scannable without burying everything.
  const [openDir, setOpenDir] = useState(depth === 0);
  const pad = { paddingLeft: `${depth * 12 + 8}px` };

  if (node.type === "dir") {
    return (
      <li>
        <button className="node dir" style={pad} onClick={() => setOpenDir((o) => !o)}>
          <span className="caret">{openDir ? "▾" : "▸"}</span>
          {node.name}
        </button>
        {openDir && (
          <ul>
            {node.children.map((c) => (
              <TreeItem
                key={c.path}
                node={c}
                depth={depth + 1}
                repo={repo}
                openPath={openPath}
                onOpenFile={onOpenFile}
              />
            ))}
          </ul>
        )}
      </li>
    );
  }

  return (
    <li>
      <button
        className={node.path === openPath ? "node file active" : "node file"}
        style={pad}
        onClick={() => onOpenFile(repo, node.path)}
      >
        <span className="caret" />
        {node.name}
      </button>
    </li>
  );
}
