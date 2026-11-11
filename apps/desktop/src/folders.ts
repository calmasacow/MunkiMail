export type ServerFolder = { account: string; name: string; delimiter: string | null; selectable: boolean };
export type FolderNode = { name: string; label: string; folder?: ServerFolder; children: FolderNode[] };
// IMAP wire names remain untouched; decode modified UTF-7 only for display.
export function folderLabel(name: string): string {
  return name.replace(/&([^-]*)-/g, (wire, encoded: string) => {
    if (!encoded) return "&";
    try {
      const bytes = atob(encoded.replaceAll(",", "/"));
      if (bytes.length % 2) return wire;
      const units = Array.from({ length: bytes.length / 2 }, (_, i) => (bytes.charCodeAt(i * 2) << 8) | bytes.charCodeAt(i * 2 + 1));
      return units.map((u) => String.fromCharCode(u)).join("");
    } catch { return wire; }
  });
}
export function folderTree(folders: ServerFolder[]): FolderNode[] {
  const roots: FolderNode[] = [];
  for (const folder of folders) {
    const parts = folder.delimiter ? folder.name.split(folder.delimiter) : [folder.name];
    let children = roots, path = "";
    for (const [i, part] of parts.entries()) {
      path = i ? path + folder.delimiter + part : part;
      let node = children.find((n) => n.name === path);
      if (!node) { node = { name: path, label: folderLabel(part), children: [] }; children.push(node); }
      if (i === parts.length - 1) node.folder = folder;
      children = node.children;
    }
  }
  const sort = (nodes: FolderNode[]) => { nodes.sort((a, b) => a.label.localeCompare(b.label)); nodes.forEach((n) => sort(n.children)); };
  sort(roots); return roots;
}
