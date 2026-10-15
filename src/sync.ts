import type { EditorChange, Snapshot } from "./types";

// Pure reducer: undefined requests authoritative recovery, never partial application.
export function applyChange(
  current: Snapshot | null,
  change: EditorChange,
): Snapshot | undefined {
  if (current && change.revision <= current.revision) return current;
  if (
    !current ||
    change.base_revision !== current.revision ||
    change.revision !== change.base_revision + 1
  )
    return;
  let document = change.reset;
  if (!document) {
    const objects = change.patches.length
      ? current.document.objects.slice()
      : current.document.objects;
    for (const patch of change.patches) {
      if (
        !Number.isInteger(patch.start) ||
        !Number.isInteger(patch.remove) ||
        patch.start < 0 ||
        patch.remove < 0 ||
        patch.start + patch.remove > objects.length
      )
        return;
      objects.splice(patch.start, patch.remove, ...patch.objects);
    }
    document = { ...current.document, objects, selected: change.selected };
  }
  const {
    base_revision: _base,
    patches: _patches,
    reset: _reset,
    selected: _selected,
    ...metadata
  } = change;
  return { ...metadata, document };
}
