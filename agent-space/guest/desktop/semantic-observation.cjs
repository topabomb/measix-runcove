"use strict";

// A bounded, identity-aware presentation of the accessibility tree. Plain text
// belongs to its source subtree; equal strings in unrelated regions are distinct.
const { clipText } = require("./observation.cjs");
const CONTEXT = new Set(["heading", "dialog", "alertdialog", "alert", "status",
  "tooltip", "menu", "listbox", "tree", "region", "tabpanel", "iframe",
  "form", "list", "listitem", "table", "row", "group", "navigation", "main"]);
const MAX_ROWS = 150, MAX_CHARS = 24000;
const normalize = text => String(text || "").replace(/\s+/g, " ").trim();

function build(nodes, interactive, panels = [], incomplete = false, includeText = false) {
  const entries = [], byNode = new Map();
  let truncated = incomplete;
  for (const n of nodes) {
    const actionable = interactive.has(n.role) || n.attrs.cursor === "pointer";
    const context = CONTEXT.has(n.role);
    let ancestor = n.parent, parent = null;
    while (ancestor) {
      if (byNode.has(ancestor)) { parent = byNode.get(ancestor); break; }
      ancestor = ancestor.parent;
    }
    let name = normalize(n.name), text = normalize(n.text);
    if (!actionable && !context) {
      text ||= name;
      if (!text) continue;
      // Compact mode keeps short local text plus all status/error content.
      // Optional prose is still part of this tree, never a second text list.
      let message = false;
      for (let a = n.parent; a; a = a.parent)
        if (["alert", "status", "dialog", "alertdialog", "tooltip"].includes(a.role)) message = true;
      if (!includeText && !message && Array.from(text).length > 300) continue;
      // Only ancestors of THIS source may subsume its text. Do not deduplicate
      // by global string membership or discard independent sibling messages.
      let covered = false;
      for (let p = parent; p; p = p.parent) {
        if (p.sourceText && (p.sourceText === text ||
            (p.actionable && p.sourceText.includes(text)))) { covered = true; break; }
      }
      if (covered) continue;
      name = "";
    }
    if (actionable && !n.ref) { truncated = true; continue; }
    const role = actionable || context ? n.role : "text";
    const content = name || text;
    if (Array.from(content).length > 300 || Array.from(text).length > 300) truncated = true;
    let line = (n.ref && (actionable || context) ? n.ref + " " : "") + role;
    if (content) line += " " + JSON.stringify(clipText(content, 300));
    if ((actionable || context) && name && text && text !== name)
      line += " =" + JSON.stringify(clipText(text, 300));
    for (const key of ["checked", "disabled", "expanded", "pressed", "selected", "level", "modal"])
      if (n.attrs[key] !== undefined)
        line += " [" + key + (n.attrs[key] === true ? "" : "=" + n.attrs[key]) + "]";
    const entry = { key: n.ref || "", line, parent, actionable, sourceText: normalize(name || text), node: n };
    entries.push(entry); byNode.set(n, entry);
  }
  // Real dialogs/popovers sometimes have no corresponding AX container. Group
  // their observed descendants without repeating labels/text or guessing an opener.
  for (const panel of panels) {
    let panelName = normalize(panel.name);
    if (Array.from(panelName).length > 300) truncated = true;
    panelName = clipText(panelName, 300);
    const members = new Set(panel.refs || []);
    const root = entries.find(e => e.key === panel.ref);
    if (root) {
      if (!root.node?.name && panelName) {
        const prefix = (root.node?.ref ? root.node.ref + " " : "") + root.node.role;
        root.line = prefix + " " + JSON.stringify(panelName) + root.line.slice(prefix.length);
      }
      if (panel.modal && !root.line.includes("[modal]")) root.line += " [modal]";
      continue;
    }
    const children = entries.filter(e => members.has(e.key));
    if (!children.length) continue;
    // An existing semantic container enclosing all members already conveys it.
    const shared = [];
    for (let p = children[0].parent; p; p = p.parent) shared.push(p);
    if (shared.some(p => ["dialog", "alertdialog", "menu", "listbox", "tooltip"].includes(p.node?.role) &&
      children.every(e => { for (let a = e.parent; a; a = a.parent) if (a === p) return true; return false; }))) continue;
    const parent = shared.find(p => children.every(e => {
      for (let a = e.parent; a; a = a.parent) if (a === p) return true;
      return false;
    })) || null;
    const group = { key: panel.ref, line: panel.role + (panelName ? " " + JSON.stringify(panelName) : "") +
      (panel.modal ? " [modal]" : ""), parent, sourceText: "" };
    const first = Math.min(...children.map(e => entries.indexOf(e)));
    entries.splice(first, 0, group);
    const selected = new Set(children);
    for (const child of children) {
      // Only move observed members, never their non-member ancestors/siblings.
      let hasSelectedAncestor = false;
      for (let a = child.parent; a; a = a.parent)
        if (selected.has(a)) { hasSelectedAncestor = true; break; }
      if (!hasSelectedAncestor) child.parent = group;
    }
  }
  // Anonymous redundant wrappers must not multiply identical text. Child text
  // under named controls was handled above; source identity remains internal.
  const siblingCounts = new Map();
  for (const e of entries) {
    if (!e.key) {
      const prefix = (e.parent?.key || "root") + "/" + (e.node?.role || "panel");
      const index = siblingCounts.get(prefix) || 0;
      siblingCounts.set(prefix, index + 1);
      e.key = prefix + "/" + index;
    }
  }
  const rows = [];
  const emitted = new Set();
  let chars = 0;
  function emit(e) {
    if (emitted.has(e)) return true;
    if (e.parent && !emit(e.parent)) return false;
    const depth = e.parent ? rows.find(r => r.key === e.parent.key).depth + 1 : 0;
    const line = "  ".repeat(depth) + e.line;
    if (rows.length >= MAX_ROWS || chars + line.length > MAX_CHARS) { truncated = true; return false; }
    rows.push({ key: e.key, parent: e.parent?.key || null, depth, line });
    chars += line.length + 1; emitted.add(e); return true;
  }
  for (const e of entries) if (!emit(e)) break;
  return { rows, truncated };
}

function present(current, previous, mode, identity) {
  const snapshot = () => ({ observation: current.rows.map(r => r.line).join("\n"),
    ...(current.truncated ? { observation_truncated: true } : {}) });
  if (mode !== "diff" || current.truncated || !previous || previous.truncated ||
      previous.document !== identity.document || previous.url !== identity.url || previous.mode !== identity.mode)
    return snapshot();
  const before = new Map(previous.rows.map(r => [r.key, r]));
  const after = new Map(current.rows.map(r => [r.key, r]));
  // Reordering is observable even when names and parents did not change. A
  // fresh tree is clearer than presenting an unchanged/ambiguous line patch.
  if (previous.rows.filter(r => after.has(r.key)).map(r => r.key).join("\n") !==
      current.rows.filter(r => before.has(r.key)).map(r => r.key).join("\n")) return snapshot();
  // A moved subtree is clearer as a fresh tree than two ambiguous contexts.
  if (current.rows.some(r => before.has(r.key) && before.get(r.key).parent !== r.parent)) return snapshot();
  const output = [], context = new Set();
  const addContext = (row, map) => {
    const parent = map.get(row.parent);
    if (!parent || context.has(parent.key)) return;
    addContext(parent, map);
    // Changed ancestors get their own +/- pair, not a misleading unchanged line.
    if (before.get(parent.key)?.line === after.get(parent.key)?.line) {
      output.push("  " + parent.line); context.add(parent.key);
    }
  };
  // Merge in tree order so removals stay beside their original siblings,
  // rather than appearing at the end under a different region's context.
  let oldIndex = 0;
  const remove = row => {
    addContext(row, before); output.push("- " + row.line); context.add(row.key);
  };
  for (const row of current.rows) {
    const old = before.get(row.key);
    if (old) {
      while (previous.rows[oldIndex].key !== row.key) remove(previous.rows[oldIndex++]);
      oldIndex++;
    }
    if (old?.line === row.line) continue;
    addContext(row, after);
    if (old) output.push("- " + old.line);
    output.push("+ " + row.line); context.add(row.key);
  }
  while (oldIndex < previous.rows.length) remove(previous.rows[oldIndex++]);
  // A large patch is less useful than the current bounded tree.
  if (output.length > MAX_ROWS || output.join("\n").length > MAX_CHARS) return snapshot();
  return { observation_diff: output.join("\n") };
}
module.exports = { build, present };
