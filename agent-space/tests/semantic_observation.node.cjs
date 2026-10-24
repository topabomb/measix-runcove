"use strict";
const { test } = require("node:test"), assert = require("node:assert/strict");
const { build, present } = require("../guest/desktop/semantic-observation.cjs");
const interactive = new Set(["button", "textbox"]);
const identity = { document: "doc", url: "https://fixture.test", mode: "compact" };
const node = (ref, role, name, parent = null, attrs = {}) => ({ ref, role, name, parent, attrs });
const snapshot = nodes => build(nodes, interactive);
const baseline = tree => ({ ...identity, ...tree });

test("deduplication is source-local; independent equal messages survive with context", () => {
  const region = node("r", "region", "Upload"), button = node("b", "button", "Upload file", region);
  const other = node("o", "region", "History");
  const tree = snapshot([region, button, node("", "text", "Upload file", button), other,
    node("", "text", "Upload file", other)]);
  assert.equal(tree.rows.filter(r => r.line.includes('"Upload file"')).length, 2);
  assert.match(tree.rows.at(-1).line, /^  text/);
  assert.equal(tree.rows.at(-1).parent, "o");
});
test("changes expose old and new descriptions; removals retain names and context", () => {
  const region = node("r", "region", "Upload"), old = node("b", "button", "Add files", region);
  const before = snapshot([region, old, node("c", "button", "Cancel", region)]);
  const after = snapshot([region, node("b", "button", "Add files", region, {expanded: true})]);
  const result = present(after, baseline(before), "diff", identity);
  assert.match(result.observation_diff, /-   b button "Add files"/);
  assert.match(result.observation_diff, /\+   b button "Add files" \[expanded\]/);
  assert.match(result.observation_diff, /-   c button "Cancel"/);
  assert.match(result.observation_diff, /region "Upload"/);
});
test("unchanged, navigation, reorder and incomplete observations have distinct outputs", () => {
  const a = node("a", "button", "A"), b = node("b", "button", "B");
  const tree = snapshot([a,b]);
  assert.deepEqual(present(tree, baseline(tree), "diff", identity), {observation_diff:""});
  assert.ok(present(tree, baseline(tree), "diff", {...identity, document:"new"}).observation);
  assert.ok(present(snapshot([b,a]), baseline(tree), "diff", identity).observation);
  assert.ok(present(tree, {...baseline(tree), truncated:true}, "diff", identity).observation);
  assert.ok(present({...tree,truncated:true}, baseline(tree), "diff", identity).observation_truncated);
});
test("large trees and long text never imply reliable removals", () => {
  const tree = snapshot(Array.from({length:200}, (_,i)=>node("b"+i,"button","Item "+i)));
  assert.equal(tree.truncated, true); assert.equal(tree.rows.length,150);
  const long = build([node("", "text", "🙂".repeat(500))], interactive, [], false, true);
  assert.equal(long.truncated,true); assert.ok(!long.rows[0].line.includes("\ufffd"));
});
test("optional prose filtering does not disable compact diffs; long alerts remain explicit", () => {
  const control = node("b", "button", "Continue");
  const prose = node("", "text", "article ".repeat(100));
  const compact = snapshot([control, prose]);
  assert.equal(compact.truncated, false);
  assert.equal(compact.rows.length, 1);
  assert.equal(present(compact, baseline(compact), "diff", identity).observation_diff, "");
  const alert = node("a", "alert", "error ".repeat(100));
  assert.equal(snapshot([alert]).truncated, true);
});
test("anonymous popover groups controls without copying their labels", () => {
  const nodes = [node("a","button","A"),node("b","button","B")];
  const tree = build(nodes, interactive, [{ref:"p",role:"panel",refs:["a","b"]}]);
  assert.deepEqual(tree.rows.map(r=>r.line), ['panel','  a button "A"','  b button "B"']);
});


test("removals stay in tree order beside their own region", () => {
  const a = node("a", "region", "A"), b = node("b", "region", "B");
  const before = snapshot([a,node("x","button","Removed",a),b,node("y","button","Old",b)]);
  const after = snapshot([a,b,node("y","button","New",b)]);
  const diff = present(after, baseline(before), "diff", identity).observation_diff;
  assert.ok(diff.indexOf('Removed') < diff.indexOf('region "B"'), diff);
  assert.match(diff, /region "A"\n-   x button "Removed"/);
});

test("panel grouping never pulls in unrelated siblings and moving subtrees snapshots", () => {
  const a = node("a", "region", "A"), b = node("b", "region", "B");
  const tree = build([a,node("x","button","Inside",a),node("z","button","Outside",a),b,node("y","button","Inside",b)],
    interactive,[{ref:"p",role:"panel",refs:["x","y"]}]);
  const rows = new Map(tree.rows.map(r=>[r.key,r]));
  assert.equal(rows.get("z").parent,"a");assert.equal(rows.get("a").parent,null);
  assert.equal(rows.get("x").parent,"p");assert.equal(rows.get("y").parent,"p");
  const before = snapshot([a,node("x","button","Move",a),b]);
  const after = snapshot([a,b,node("x","button","Move",b)]);
  assert.ok(present(after,baseline(before),"diff",identity).observation);
});


test("DOM name enrichment keeps the observed role and states and bounds text", () => {
  const tree = build([node("b","button","",null,{expanded:true})],interactive,
    [{ref:"b",role:"panel",name:"Open"}]);
  assert.equal(tree.rows[0].line,'b button "Open" [expanded]');
  const long = build([node("b","button","B")],interactive,
    [{ref:"p",role:"panel",name:"🙂".repeat(500),refs:["b"]}]);
  assert.equal(long.truncated,true);assert.ok(!long.rows[0].line.includes("\ufffd"));
  assert.ok(Array.from(long.rows[0].line).length<320);
});
