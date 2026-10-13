export type WorldAnchor = {
  desktop: readonly [x: number, y: number];
  compact: readonly [x: number, y: number];
  rotation: number;
};

export const WORLD_ANCHORS = {
  opnest: { desktop: [18, 20], compact: [28, 9.15], rotation: -5 },
  miniBrowser: { desktop: [35.5, 21], compact: [79, 10.7], rotation: -7 },
  palette: { desktop: [45.5, 20], compact: [51, 14.7], rotation: 2 },
  paperSketch: { desktop: [55, 20], compact: [55, 20], rotation: 5 },
  eyeStudy: { desktop: [72, 19], compact: [72, 19], rotation: -3 },
  darkThoughtful: { desktop: [86, 20], compact: [85, 18.2], rotation: 5 },
  blackSketch: { desktop: [4, 30], compact: [9, 19.4], rotation: 4 },
  leftImageStack: { desktop: [18, 37], compact: [37, 19.6], rotation: -3 },
  japanesePoster: { desktop: [4, 47], compact: [7, 31.2], rotation: -7 },
  hero: { desktop: [50, 42], compact: [50, 28], rotation: 0 },
  rightImageStack: { desktop: [79, 43], compact: [77, 48.8], rotation: -4 },
  noteCard: { desktop: [80, 54], compact: [77, 55.5], rotation: -7 },
  humanSite: { desktop: [12, 64], compact: [17, 67.6], rotation: 5 },
  thoughtCard: { desktop: [8, 76], compact: [17, 77], rotation: 3 },
  wordmark: { desktop: [49, 66], compact: [50, 39.1], rotation: 0 },
  processSketch: { desktop: [16, 90], compact: [16, 90.9], rotation: -2 },
  editorialFragment: { desktop: [35.5, 88], compact: [38.5, 83.9], rotation: -1 },
  terminal: { desktop: [49, 89], compact: [82, 81.8], rotation: 2 },
  buildCard: { desktop: [91, 55], compact: [41, 49.4], rotation: 3 },
  ideasSite: { desktop: [81, 84], compact: [81, 68.2], rotation: -7 },
} as const satisfies Record<string, WorldAnchor>;

export type WorldRegion = keyof typeof WORLD_ANCHORS;

export const SCENE_ANCHORS = {
  home: "home",
  gallery: "rightImageStack",
  features: "processSketch",
  useCases: "ideasSite",
} as const satisfies Record<string, WorldRegion | "home">;

export const SCENE_NAV_ITEMS = ["gallery", "features", "useCases"] as const satisfies readonly (keyof typeof SCENE_ANCHORS)[];

export type SceneAnchor = keyof typeof SCENE_ANCHORS;

export function anchorPosition(region: WorldRegion, compact = false) {
  const [x, y] = WORLD_ANCHORS[region][compact ? "compact" : "desktop"];
  return { x, y, rotation: WORLD_ANCHORS[region].rotation };
}
