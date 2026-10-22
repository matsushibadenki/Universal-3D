import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import ts from "typescript";
const load = async (path) =>
  import(
    `data:text/javascript;base64,${Buffer.from(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText).toString("base64")}`
  );
const { applyChange } = await load("../src/sync.ts");
const { preview } = await load("../src/types.ts");
const event = (revision, patches = [], extra = {}) => ({
  ...preview,
  document: undefined,
  base_revision: revision - 1,
  revision,
  patches,
  reset: null,
  selected: 1,
  ...extra,
});
test("ordered update/add/delete and metadata preserve authoritative document", () => {
  const original = structuredClone(preview);
  const edited = { ...preview.document.objects[0], name: "Edited" };
  let state = applyChange(
    original,
    event(1, [{ start: 0, remove: 1, objects: [edited] }]),
  );
  assert.equal(original.document.objects[0].name, "Cube");
  assert.equal(state.document.objects[0].name, "Edited");
  const second = { ...edited, id: 2 };
  state = applyChange(
    state,
    event(2, [{ start: 1, remove: 0, objects: [second] }], { selected: 2 }),
  );
  state = applyChange(
    state,
    event(3, [{ start: 0, remove: 1, objects: [] }], { selected: 2 }),
  );
  assert.deepEqual(state.document.objects, [second]);
  const objects = state.document.objects;
  state = applyChange(state, event(4, [], { selected: 2, frame: 42 }));
  assert.equal(state.document.objects, objects);
  assert.equal(state.frame, 42);
});
test("duplicate/stale events ignored; initial and missing sequence require resync", () => {
  const state = applyChange(preview, event(1));
  assert.equal(applyChange(state, event(1)), state);
  assert.equal(applyChange(state, event(0)), state);
  assert.equal(applyChange(state, event(3)), undefined);
  assert.equal(applyChange(null, event(1)), undefined);
  // Authoritative resync at revision 3 replaces local state; queued old events are ignored.
  const recovered = { ...preview, revision: 3 };
  assert.equal(applyChange(recovered, event(2)), recovered);
  assert.equal(applyChange(recovered, event(4)).revision, 4);
});
test("reset and sequential coalesced splices; invalid patch is atomic", () => {
  const state = applyChange(
    preview,
    event(1, [], {
      reset: { version: 1, objects: [], selected: null },
      selected: null,
    }),
  );
  assert.deepEqual(state.document.objects, []);
  const object = preview.document.objects[0];
  const next = applyChange(
    state,
    event(
      2,
      [
        { start: 0, remove: 0, objects: [object] },
        { start: 0, remove: 1, objects: [] },
      ],
      { selected: null },
    ),
  );
  assert.deepEqual(next.document.objects, []);
  assert.equal(
    applyChange(
      preview,
      event(1, [
        { start: 0, remove: 1, objects: [] },
        { start: 3, remove: 1, objects: [] },
      ]),
    ),
    undefined,
  );
  assert.equal(preview.document.objects.length, 1);
});

test("panel/viewport close updates metadata without replacing the document", () => {
  const initial = { ...preview, viewport: true, floating: ["inspector"] };
  const closed = applyChange(
    initial,
    event(1, [], { viewport: false, floating: [] }),
  );
  assert.equal(closed.viewport, false);
  assert.deepEqual(closed.floating, []);
  assert.equal(closed.document.objects, initial.document.objects);
});

for (const [tool, field, value] of [
  ["move", "position", [2, 0, 0]],
  ["rotate", "rotation", [0, 45, 90]],
  ["scale", "scale", [-2, 3, 4]],
]) {
  test(`${tool} preview, cancellation and tool metadata reach every panel`, () => {
    const original = preview.document.objects[0];
    const changed = { ...original, [field]: value };
    let state = applyChange(preview, event(1, [], { tool, dragging: true }));
    assert.equal(state.document.objects, preview.document.objects);
    assert.equal(state.dragging, true);
    state = applyChange(state, event(2, [{ start: 0, remove: 1, objects: [changed] }], {
      tool, dragging: true, dirty: true, can_undo: false,
    }));
    assert.deepEqual(state.document.objects[0][field], value);
    state = applyChange(state, event(3, [{ start: 0, remove: 1, objects: [original] }], {
      tool, dragging: false, dirty: false, can_undo: false,
    }));
    assert.deepEqual(state.document, preview.document);
    assert.equal(state.tool, tool);
    assert.equal(state.dragging, false);
    assert.equal(state.dirty, false);
  });
}


test("coordinate and snap settings synchronize without copying objects; shear survives patches", () => {
  const settings={...preview.transform_settings, move_space:"local", scale_space:"world", snap:true, rotate_step:30};
  const configured=applyChange(preview,event(1,[],{transform_settings:settings}));
  assert.deepEqual(configured.transform_settings,settings);
  assert.equal(configured.document.objects,preview.document.objects);
  const sheared={...preview.document.objects[0],shear:[0.5,-0.2,0.3]};
  const changed=applyChange(configured,event(2,[{start:0,remove:1,objects:[sheared]}],{transform_settings:settings}));
  assert.deepEqual(changed.document.objects[0].shear,sheared.shear);
  assert.deepEqual(changed.transform_settings,settings);
});
