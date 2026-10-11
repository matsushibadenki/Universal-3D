export type SceneObject = {
  id: number;
  name: string;
  kind: string;
  position: number[];
  rotation: number[];
  scale: number[];
  visible: boolean;
  mirror: boolean;
};
export type Snapshot = {
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
};
export type Command = { type: string; [key: string]: unknown };
export const preview: Snapshot = {
  document: {
    version: 1,
    objects: [
      {
        id: 1,
        name: "Cube",
        kind: "cube",
        position: [0, 0, 0],
        rotation: [0, 0, 0],
        scale: [1, 1, 1],
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
};
