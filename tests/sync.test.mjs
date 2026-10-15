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
