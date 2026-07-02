// Build a nested directory tree from a flat list of repo-relative file paths
// (as returned by `git ls-files`), for the sidebar's collapsible browser.

export interface DirNode {
  type: "dir";
  name: string;
  path: string; // repo-relative path of this directory
  children: TreeNode[];
}

export interface FileNode {
  type: "file";
  name: string;
  path: string; // repo-relative path of the file
}

export type TreeNode = DirNode | FileNode;

export function buildTree(files: string[]): TreeNode[] {
  const root: DirNode = { type: "dir", name: "", path: "", children: [] };

  for (const file of files) {
    const parts = file.split("/");
    let dir = root;
    // Walk/create the directory chain, then attach the file leaf.
    for (let i = 0; i < parts.length - 1; i++) {
      const name = parts[i];
      const path = dir.path ? `${dir.path}/${name}` : name;
      let next = dir.children.find(
        (c): c is DirNode => c.type === "dir" && c.name === name,
      );
      if (!next) {
        next = { type: "dir", name, path, children: [] };
        dir.children.push(next);
      }
      dir = next;
    }
    const name = parts[parts.length - 1];
    dir.children.push({ type: "file", name, path: file });
  }

  sort(root);
  return root.children;
}

// Directories before files, each group alphabetical — the conventional ordering.
function sort(node: DirNode): void {
  node.children.sort((a, b) => {
    if (a.type !== b.type) return a.type === "dir" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  for (const child of node.children) {
    if (child.type === "dir") sort(child);
  }
}
