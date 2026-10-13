"use client";

import { useEffect, useLayoutEffect, useMemo, useState } from "react";
import { buildPuzzle, getPuzzleGrid, schedulePuzzleReveal, type PuzzlePiece } from "./puzzle-layout";

export type PuzzleRevealMode = "veil" | "void";
type PuzzleStage = "reveal" | "complete" | "exit";

type PuzzleLoaderProps = {
  stage: PuzzleStage;
  mode: PuzzleRevealMode;
};

type ViewportSize = { width: number; height: number };
const VOID_MASK_ID = "puzzle-void-mask";
const VEIL_SHADOW_FILTER_ID = "puzzle-veil-shadow-filter";

function useViewportSize(): ViewportSize {
  const [size, setSize] = useState<ViewportSize>({ width: 0, height: 0 });

  useLayoutEffect(() => {
    const viewport = document.querySelector<HTMLElement>("[data-camera-viewport]");
    if (!viewport) return;

    const update = () => {
      const bounds = viewport.getBoundingClientRect();
      setSize({ width: bounds.width, height: bounds.height });
    };

    update();
    const observer = new ResizeObserver(update);
    observer.observe(viewport);
    return () => observer.disconnect();
  }, []);

  return size;
}


type PieceRevealRegistry = Map<string, Set<SVGElement>>;

function registerRevealElement(registry: PieceRevealRegistry, pieceId: string, element: SVGElement | null) {
  const elements = registry.get(pieceId) ?? new Set<SVGElement>();
  if (element) elements.add(element);
  else {
    for (const registeredElement of elements) {
      if (!registeredElement.isConnected) elements.delete(registeredElement);
    }
  }
  if (elements.size) registry.set(pieceId, elements);
  else registry.delete(pieceId);
}

function pieceTransform(piece: PuzzlePiece, grid: ReturnType<typeof getPuzzleGrid>, width: number, height: number) {
  const cellWidth = width / grid.columns;
  const cellHeight = height / grid.rows;
  const left = piece.column * cellWidth;
  const top = piece.row * cellHeight;

  return `translate(${left} ${top}) scale(${cellWidth} ${cellHeight}) rotate(${piece.rotation})`;
}

function PuzzlePieces({
  pieces,
  stage,
  grid,
  viewport,
  mode,
  revealRegistry,
}: {
  pieces: PuzzlePiece[];
  stage: PuzzleStage;
  grid: ReturnType<typeof getPuzzleGrid>;
  viewport: ViewportSize;
  mode: PuzzleRevealMode;
  revealRegistry: PieceRevealRegistry;
}) {
  const initialRevealState = stage === "reveal" ? "false" : "true";
  const width = Math.max(viewport.width, 1);
  const height = Math.max(viewport.height, 1);

  return (
    <svg
      className="puzzle-shape-layer"
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      <defs>
        {mode === "void" ? (
          <mask
            id={VOID_MASK_ID}
            maskUnits="userSpaceOnUse"
            maskContentUnits="userSpaceOnUse"
            x="0"
            y="0"
            width={width}
            height={height}
          >
            <rect width={width} height={height} fill="black" />
            {pieces.map((piece) => (
              <g key={piece.id} transform={pieceTransform(piece, grid, width, height)}>
                <path
                  d={piece.path}
                  className="puzzle-void-window"
                  data-piece-id={piece.id}
                  data-revealed={initialRevealState}
                  ref={(path) => registerRevealElement(revealRegistry, piece.id, path)}
                  fill="white"
                />
              </g>
            ))}
          </mask>
        ) : null}
        {mode === "veil" ? (
          <>
            <filter
              id={VEIL_SHADOW_FILTER_ID}
              x="-35%"
              y="-35%"
              width="170%"
              height="170%"
              colorInterpolationFilters="sRGB"
            >
              <feGaussianBlur in="SourceAlpha" stdDeviation=".006" result="contact-blur" />
              <feOffset in="contact-blur" dx=".006" dy=".008" result="contact-offset" />
              <feFlood className="puzzle-veil-contact-shadow" floodColor="#1c1b18" floodOpacity=".2" result="contact-color" />
              <feComposite in="contact-color" in2="contact-offset" operator="in" result="contact-shadow" />
              <feGaussianBlur in="SourceAlpha" stdDeviation=".018" result="ambient-blur" />
              <feOffset in="ambient-blur" dx=".008" dy=".025" result="ambient-offset" />
              <feFlood className="puzzle-veil-ambient-shadow" floodColor="#1c1b18" floodOpacity=".1" result="ambient-color" />
              <feComposite in="ambient-color" in2="ambient-offset" operator="in" result="ambient-shadow" />
              <feMerge result="combined-shadow">
                <feMergeNode in="contact-shadow" />
                <feMergeNode in="ambient-shadow" />
              </feMerge>
              <feComposite in="combined-shadow" in2="SourceAlpha" operator="out" />
            </filter>
            {pieces.map((piece) => (
              <clipPath id={`puzzle-veil-clip-${piece.id}`} key={piece.id} clipPathUnits="userSpaceOnUse">
                <path d={piece.path} />
              </clipPath>
            ))}
          </>
        ) : null}
      </defs>
      {mode === "veil" ? pieces.map((piece) => (
        <g key={`shadow-${piece.id}`} transform={pieceTransform(piece, grid, width, height)}>
          <path
            d={piece.path}
            className="puzzle-veil-shadow"
            data-piece-id={piece.id}
            data-revealed={initialRevealState}
            ref={(path) => registerRevealElement(revealRegistry, piece.id, path)}
            fill="#1c1b18"
            filter={`url(#${VEIL_SHADOW_FILTER_ID})`}
          />
        </g>
      )) : null}
      {mode === "veil" ? pieces.map((piece) => (
        <g
          key={piece.id}
          className="puzzle-veil-piece"
          data-piece-id={piece.id}
          data-revealed={initialRevealState}
          ref={(element) => registerRevealElement(revealRegistry, piece.id, element)}
          transform={pieceTransform(piece, grid, width, height)}
        >
          <path d={piece.path} fill="#f4efe5" />
          <g clipPath={`url(#puzzle-veil-clip-${piece.id})`}>
            <image
              href="/textures/paper-grain.svg"
              x={-piece.column}
              y={-piece.row}
              width={grid.columns}
              height={grid.rows}
              preserveAspectRatio="none"
              opacity=".34"
              transform={`rotate(${-piece.rotation})`}
            />
          </g>
        </g>
      )) : null}
    </svg>
  );
}

export default function PuzzleLoader({ stage, mode }: PuzzleLoaderProps) {
  const viewport = useViewportSize();
  const hasViewport = viewport.width > 0 && viewport.height > 0;
  const compact = viewport.width > 0 && viewport.width <= 720;
  const grid = getPuzzleGrid(compact);
  const pieces = useMemo(
    () => hasViewport ? schedulePuzzleReveal(buildPuzzle(compact)) : [],
    [compact, hasViewport],
  );
  const revealRegistry = useMemo<PieceRevealRegistry>(() => new Map(), []);

  useEffect(() => {
    if (stage !== "reveal") return;

    const timers = pieces.map((piece) => window.setTimeout(() => {
      revealRegistry.get(piece.id)?.forEach((element) => {
        element.setAttribute("data-revealed", "true");
      });
    }, piece.enterDelay));

    return () => timers.forEach(window.clearTimeout);
  }, [revealRegistry, pieces, stage]);


  return (
    <div className="puzzle-layer" data-puzzle-stage={stage} data-puzzle-mode={mode} aria-hidden="true">
      <PuzzlePieces
        pieces={pieces}
        stage={stage}
        grid={grid}
        viewport={viewport}
        mode={mode}
        revealRegistry={revealRegistry}
      />
    </div>
  );
}
