// Pure tree helpers for the team's workspace — a real, durable file tree of
// folders and markdown notes that lives on the **host**, in the company's
// `WorkspaceStore`. The console reads and writes it over the `…/workspace`
// routes in `@/api/workspace`; the company's agents read and write the same
// tree through their workspace tools, so operator and agents share one surface.
//
// Nothing here persists anything. These are the derivations the view needs on
// top of a tree it has already fetched (ordering, ancestry, wiki-link title
// resolution). The one localStorage touch left is the *migration* pair at the
// bottom, which exists solely to rescue notes typed into the retired
// client-side scratchpad and is expected to become a no-op once every browser
// has been through it once.

export type { FsNode } from "@/api/workspace";

import {
  type LocalScope,
  scopedKey,
  scopedKeyAdoptingLegacy,
} from "@/connections/types";

import type { FsNode, RepairOutcome } from "@/api/workspace";

/* ---- queries ---- */

export function childrenOf(nodes: FsNode[], parentId: string | null): FsNode[] {
  return nodes
    .filter((x) => x.parentId === parentId)
    .sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "folder" ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
}

export function nodeById(
  nodes: FsNode[],
  id: string | null,
): FsNode | undefined {
  return id ? nodes.find((x) => x.id === id) : undefined;
}

/**
 * A file's display title — its name without the markdown extension.
 *
 * Known divergence: this strips `.md`, `.markdown` and `.txt`, while the host's
 * `link_target` (which computes backlinks) strips only `.md`. So a `[[link]]`
 * to a `.txt` note styles as resolved here but is not counted as a backlink
 * server-side. Cosmetic, and deliberately left alone — the seeder only ever
 * creates `.md` notes, and narrowing this would silently restyle existing links.
 */
export function titleOf(node: FsNode): string {
  return node.name.replace(/\.(md|markdown|txt)$/i, "");
}

/** Resolve an Obsidian-style `[[wiki link]]` target to a file, by title. */
export function fileByTitle(
  nodes: FsNode[],
  target: string,
): FsNode | undefined {
  const want = target.trim().toLowerCase();
  return nodes.find(
    (x) => x.kind === "file" && titleOf(x).toLowerCase() === want,
  );
}

/**
 * The folder whose contents are written by code and never by hand.
 *
 * Kept in step with `DERIVED_DIR` in `src/ledger/spec.rs`. If the host ever
 * renames it, a write refused here would be one the console still offered.
 */
export const DERIVED_DIR = "derived";

/**
 * Whether the node at `id` is one the host will refuse to let a person write
 * (issue #1222).
 *
 * A **folder** rule, mirroring `is_derived_path` in `src/ledger/derived.rs`
 * character for character — including the case-insensitive comparison, because
 * "a guard that can be stepped around by capitalising a letter is not a guard".
 * That module's header argues why the invariant is the folder and not the file:
 * a per-file rule fails open, because a ledger declared next week renders a file
 * no list has heard of.
 *
 * The console applies the same rule rather than reading a flag because the wire
 * carries none — `GET …/workspace` returns no `readOnly` — and because a rule
 * evaluated from the same fact on both sides cannot drift the way two lists
 * would. It is what lets the pane refuse *before* the typing instead of after,
 * which is the whole of #1222.
 */
export function isDerivedNode(nodes: FsNode[], id: string | null): boolean {
  const ancestry = pathOf(nodes, id);
  const head = ancestry[0];
  if (!head) return false;
  return (
    head.kind === "folder" && head.name.trim().toLowerCase() === DERIVED_DIR
  );
}

/**
 * The same rule as {@link isDerivedNode}, applied to a **path string** rather
 * than a tree (issue #1377).
 *
 * The search hit list has no tree to walk — it is a flat list of results, and
 * the nodes it names may sit in folders the explorer has never expanded. But
 * every hit carries its own `path`, so the folder rule can be read straight off
 * that. Kept beside `isDerivedNode` and written from the same `DERIVED_DIR`
 * constant, so the two cannot drift into disagreeing about which files a person
 * may write.
 *
 * Leading slashes are tolerated for the same reason `is_derived_path` tolerates
 * them: the guard must not be steppable around by a formatting difference.
 */
export function isDerivedPath(path: string): boolean {
  const head = path.trim().replace(/^\/+/, "").split("/")[0];
  return head !== undefined && head.trim().toLowerCase() === DERIVED_DIR;
}

/**
 * What the tree, the search list and the note header all call a `derived/`
 * file (issue #1377).
 *
 * One phrase in three places on purpose. "Read only" — what the header chip
 * used to say — reports that an edit is unwelcome without saying *why*, and a
 * rule with no reason behind it is the kind of rule people work around or file
 * as a bug. "Written by a ledger" is the reason, and it is short enough to fit
 * a tree row, a search hit and a header chip alike.
 */
export const DERIVED_LABEL = "Written by a ledger";

/**
 * The long form of {@link DERIVED_LABEL} — what happens if you edit it anyway,
 * and where the edit actually belongs.
 *
 * Lives here rather than in `WorkspaceView` because the search list is a
 * sibling module that would otherwise have to import from its own parent.
 */
export const DERIVED_REASON =
  "This file is written by a ledger and re-derived on every write to it — " +
  "an edit here would be erased. Change it on the Ledgers page instead.";

/** Ancestor folders (root → current), for breadcrumbs. */
export function pathOf(nodes: FsNode[], id: string | null): FsNode[] {
  const path: FsNode[] = [];
  let cur = nodeById(nodes, id);
  while (cur) {
    path.unshift(cur);
    cur = nodeById(nodes, cur.parentId);
  }
  return path;
}

/**
 * One step of a note's location, for the header breadcrumb (issue #1371).
 *
 * `null` is the elided middle — a crumb the trail could not fit, rendered as an
 * ellipsis rather than dropped, so the operator can see that the path is longer
 * than what is shown instead of reading a shortened path as the whole truth.
 */
export type Crumb = FsNode | null;

/**
 * The folders a note sits inside, shortened to at most `max` of them.
 *
 * The note itself is **not** in the trail: its name is already the heading beside
 * it, and repeating it would spend the widest crumb saying what the operator is
 * looking straight at.
 *
 * When the trail is too long, the **root and the last two folders** survive and
 * the middle collapses. That split is the useful one: the root says which part
 * of the company this belongs to, and the last two say what it sits next to.
 * Truncating the string instead — which is what `truncate` on a single span did
 * — ellipsises the *tail*, so every note under `Standards/Engineering/…` renders
 * the identical prefix and the discriminating end is exactly what is thrown away.
 */
export function breadcrumbOf(
  nodes: FsNode[],
  id: string | null,
  max = 3,
): Crumb[] {
  const folders = pathOf(nodes, id).slice(0, -1);
  if (folders.length <= max) return folders;
  // `max - 2` leading crumbs, the ellipsis, then the final two.
  return [
    ...folders.slice(0, Math.max(max - 2, 1)),
    null,
    ...folders.slice(-2),
  ];
}

/**
 * The ids of every folder a node is nested in — what has to be expanded for the
 * node's own row to exist in the tree (issue #1371).
 *
 * Excludes the node itself even when it is a folder: revealing a folder means
 * showing its row, not opening it.
 */
export function ancestorFolderIds(
  nodes: FsNode[],
  id: string | null,
): string[] {
  return pathOf(nodes, id)
    .slice(0, -1)
    .filter((node) => node.kind === "folder")
    .map((node) => node.id);
}

/** Ids of a node and all its descendants (for delete / move guards). */
export function subtreeIds(nodes: FsNode[], id: string): Set<string> {
  const ids = new Set<string>([id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const node of nodes) {
      if (node.parentId && ids.has(node.parentId) && !ids.has(node.id)) {
        ids.add(node.id);
        grew = true;
      }
    }
  }
  return ids;
}

/**
 * Descendant counts for `id`, split by kind and excluding the node itself —
 * for a delete confirmation to say "3 notes and 1 folder" rather than a bare
 * "everything inside it" (issue #1255). Built on {@link subtreeIds}, which
 * includes `id` itself; this one deliberately does not.
 */
export function subtreeCounts(
  nodes: FsNode[],
  id: string,
): { files: number; folders: number } {
  const ids = subtreeIds(nodes, id);
  ids.delete(id);
  let files = 0;
  let folders = 0;
  for (const node of nodes) {
    if (!ids.has(node.id)) continue;
    if (node.kind === "file") files++;
    else folders++;
  }
  return { files, folders };
}

/**
 * The tree as it stands after a duplicate-folder repair (issue #759).
 *
 * The host's answer is a list of *changes*, not a new tree, so this replays them
 * onto the copy the tab already has — the same thing the delete and move
 * handlers do, and for the same reason: a full refetch would discard the open
 * note's unsaved draft.
 *
 * Order matters. Relocations are applied first, then a removed folder takes
 * whatever is still filed under it. That is not defensive padding: the host only
 * ever deletes a folder it has just proved empty, but *this* copy of the tree can
 * be stale — an agent may have created a note in it since the last fetch — and
 * dropping the folder while keeping the phantom child would leave a row hanging
 * off a parent that no longer exists.
 */
export function applyRepair(nodes: FsNode[], outcome: RepairOutcome): FsNode[] {
  const relocated = new Map<string, string>();
  for (const folder of outcome.folders) {
    for (const child of folder.moved) relocated.set(child.id, folder.intoId);
  }
  const moved = nodes.map((node) => {
    const parentId = relocated.get(node.id);
    return parentId === undefined ? node : { ...node, parentId };
  });

  const gone = new Set<string>();
  for (const folder of outcome.folders) {
    if (folder.removed)
      for (const id of subtreeIds(moved, folder.id)) gone.add(id);
  }
  return moved.filter((node) => !gone.has(node.id));
}

/** Notes get a markdown extension unless they already carry a known one. */
export function ensureMdExt(name: string): string {
  return /\.(md|markdown|txt)$/i.test(name) ? name : `${name}.md`;
}

/* ---- migration off the retired localStorage scratchpad ---- */

/** Where the pre-connection console kept this company's scratchpad. */
const legacyWorkspaceKey = (scope: LocalScope) =>
  `oc-workspace:${scope.company ?? "single"}`;

const KEY = (scope: LocalScope) =>
  scopedKeyAdoptingLegacy("oc-workspace", scope, legacyWorkspaceKey(scope));

/**
 * Ids the *bundled seed* used. These four nodes were app source shipped to
 * every browser — marketing copy about a "Spring launch" campaign that belonged
 * to no real company. They carry zero user information, so they are dropped
 * rather than imported: pushing them into the host would inject invented
 * content into every company's genuine workspace.
 */
const SEED_ID_PREFIX = "seed-";

/**
 * Notes a user actually typed into the old client-side workspace, if any.
 *
 * Returns only nodes the *user* authored (the retired `addFile`/`addFolder`
 * minted `fs-…` ids); bundled seed nodes are filtered out. An empty array means
 * there is nothing worth rescuing — either the key is absent, unparseable, or
 * holds nothing but the seed.
 */
export function readLegacyLocalNodes(scope: LocalScope): FsNode[] {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(KEY(scope));
  } catch {
    return [];
  }
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return parsed.filter(
    (node): node is FsNode =>
      Boolean(node) &&
      typeof (node as FsNode).id === "string" &&
      typeof (node as FsNode).name === "string" &&
      ((node as FsNode).kind === "file" ||
        (node as FsNode).kind === "folder") &&
      !(node as FsNode).id.startsWith(SEED_ID_PREFIX),
  );
}

/** Whether the legacy key exists at all (so a seed-only key can be swept). */
export function hasLegacyLocal(scope: LocalScope): boolean {
  try {
    return localStorage.getItem(KEY(scope)) !== null;
  } catch {
    return false;
  }
}

/** Where a declined migration offer is remembered, per connection. */
const DECLINED_KEY = (scope: LocalScope) =>
  scopedKey("oc-workspace-migration-declined", scope);

/**
 * Remember that the operator said "not now" to the migration offer.
 *
 * A decline is deliberately *not* a discard. The banner used to offer exactly
 * two exits — import, or destroy the notes — so an operator who wanted neither
 * met the same offer on every mount, and the quietest way to make it stop was
 * the button that deleted the only copy. This is the third exit: the notes stay
 * in the browser, untouched, and the offer stops asking.
 *
 * Scoped per connection like every other console key, so declining on one host
 * cannot hide the offer on another that has never made it.
 */
export function declineLegacyImport(scope: LocalScope): void {
  try {
    localStorage.setItem(DECLINED_KEY(scope), "1");
  } catch {
    /* storage unavailable — the offer will simply be made again */
  }
}

/** Whether this connection has already declined the migration offer. */
export function legacyImportDeclined(scope: LocalScope): boolean {
  try {
    return localStorage.getItem(DECLINED_KEY(scope)) !== null;
  } catch {
    return false;
  }
}

/**
 * Drop the retired scratchpad for this company.
 *
 * Removes the **origin** as well as this connection's adopted copy. Unlike the
 * other adopted keys — where two connections each inheriting the old tour
 * progress is the intended reading — this one is *consumed* by the import. Left
 * behind, the next connection to look would adopt it and offer the same notes
 * again, and the operator would import them twice with nothing saying so.
 */
export function clearLegacyLocal(scope: LocalScope): void {
  try {
    localStorage.removeItem(KEY(scope));
    localStorage.removeItem(legacyWorkspaceKey(scope));
  } catch {
    /* storage unavailable — nothing to clear */
  }
}
