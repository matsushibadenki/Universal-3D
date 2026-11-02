export type SceneObject = {
  id: number;
  name: string;
  kind: string;
  mesh_id?: string;
  position: number[];
  rotation: number[];
  scale: number[];
  shear: number[];
  visible: boolean;
  mirror: boolean;
};
export type Snapshot = {
  revision: number;
  document: {
    version: number;
    objects: SceneObject[];
    selected: number | null;
  };
  can_undo: boolean;
  can_redo: boolean;
  dirty: boolean;
  filename: string;
  floating: string[];
  viewport: boolean;
  frame: number;
  camera: number[];
  tool: "select" | "move" | "rotate" | "scale";
  transform_settings: {
    move_space: "world" | "local";
    rotate_space: "world" | "local";
    scale_space: "world" | "local";
    snap: boolean;
    move_snap: "relative" | "grid" | "vertex" | "edge" | "face";
    move_step: number;
    rotate_step: number;
    scale_step: number;
  };
  dragging: boolean;
};
export type Command = { type: string; [key: string]: unknown };
export const preview: Snapshot = {
  revision: 0,
  document: {
    version: 2,
    objects: [
      {
        id: 1,
        name: "Cube",
        kind: "cube",
        position: [0, 0, 0],
        rotation: [0, 0, 0],
        scale: [1, 1, 1],
        shear: [0, 0, 0],
        visible: true,
        mirror: false,
      },
    ],
    selected: 1,
  },
  can_undo: false,
  can_redo: false,
  dirty: false,
  filename: "Untitled.u3d",
  floating: [],
  viewport: false,
  frame: 1,
  camera: [0.7, 0.45, 7],
  tool: "select",
  transform_settings: {
    move_space: "world",
    rotate_space: "local",
    scale_space: "local",
    snap: false,
    move_snap: "relative",
    move_step: 1,
    rotate_step: 15,
    scale_step: 0.1,
  },
  dragging: false,
};

export type EditorChange = Omit<Snapshot, "document"> & {
  base_revision: number;
  patches: { start: number; remove: number; objects: SceneObject[] }[];
  reset: Snapshot["document"] | null;
  selected: number | null;
};
