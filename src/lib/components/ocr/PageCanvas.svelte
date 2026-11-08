<script lang="ts">
  import { onMount } from "svelte";
  import { ScanLine, MousePointer2 } from "lucide-svelte";
  import {
    normalizeRectangle,
    type OcrPage,
    type OcrRegion,
  } from "$lib/ocr/model";

  type Rectangle = { x: number; y: number; w: number; h: number };
  type DrawMode =
    | "select"
    | "text"
    | "table"
    | "picture"
    | "formula"
    | "crop"
    | "recognize";
  let {
    page,
    imageUrl,
    selectedId = null,
    threshold = 0.85,
    mode = "select",
    overlays = true,
    original = false,
    zoom = 1,
    fit = "page",
    disabled = false,
    onselect,
    onrectangle,
    ongeometry,
  }: {
    page: OcrPage;
    imageUrl: (path: string) => string;
    selectedId?: string | null;
    threshold?: number;
    mode?: DrawMode;
    overlays?: boolean;
    original?: boolean;
    zoom?: number;
    fit?: "page" | "width" | "custom";
    disabled?: boolean;
    onselect: (id: string) => void;
    onrectangle: (rect: Rectangle, mode: DrawMode) => void;
    ongeometry: (id: string, rect: Rectangle) => void;
  } = $props();
  let viewport: HTMLDivElement;
  let svg = $state<SVGSVGElement>()!;
  let paneWidth = $state(700),
    paneHeight = $state(700);
  let originalWidth = $state(0),
    originalHeight = $state(0);
  let gesture = $state<{
    kind: "draw" | "move" | "resize";
    start: { x: number; y: number };
    region?: OcrRegion;
    rect: Rectangle;
    pointer: number;
  } | null>(null);
  const width = $derived(
    original && originalWidth ? originalWidth : page.width,
  );
  const height = $derived(
    original && originalHeight ? originalHeight : page.height,
  );
  const scale = $derived(
    fit === "custom"
      ? zoom
      : fit === "width"
        ? Math.max(0.05, (paneWidth - 72) / width)
        : Math.max(
            0.05,
            Math.min((paneWidth - 72) / width, (paneHeight - 64) / height),
          ),
  );
  const selected = $derived(page.regions.find((r) => r.id === selectedId));
  const canDraw = $derived(!disabled && !original);
  const hint = $derived(
    original
      ? "Original image · overlays hidden"
      : mode === "select"
        ? "Select a region to proofread. Drag to move; use the corner to resize."
        : mode === "crop"
          ? "Draw the page area to keep. The original remains available."
          : mode === "recognize"
            ? "Draw a zone to recognize. Existing text outside it is preserved."
            : `Draw a ${mode} region on the page.`,
  );

  onMount(() => {
    const observer = new ResizeObserver(([entry]) => {
      paneWidth = entry.contentRect.width;
      paneHeight = entry.contentRect.height;
    });
    observer.observe(viewport);
    return () => observer.disconnect();
  });
  function point(event: PointerEvent) {
    const rect = svg.getBoundingClientRect();
    return {
      x: Math.min(
        page.width,
        Math.max(0, ((event.clientX - rect.left) / rect.width) * page.width),
      ),
      y: Math.min(
        page.height,
        Math.max(0, ((event.clientY - rect.top) / rect.height) * page.height),
      ),
    };
  }
  function beginDraw(event: PointerEvent) {
    if (!canDraw || mode === "select" || event.button !== 0) return;
    const start = point(event);
    gesture = {
      kind: "draw",
      start,
      rect: { ...start, w: 0, h: 0 },
      pointer: event.pointerId,
    };
    svg.setPointerCapture(event.pointerId);
    event.preventDefault();
  }
  function beginRegion(event: PointerEvent, region: OcrRegion, resize = false) {
    if (mode !== "select") return;
    event.stopPropagation();
    onselect(region.id);
    if (!canDraw || event.button !== 0) return;
    gesture = {
      kind: resize ? "resize" : "move",
      start: point(event),
      region,
      rect: { x: region.x, y: region.y, w: region.w, h: region.h },
      pointer: event.pointerId,
    };
    svg.setPointerCapture(event.pointerId);
    event.preventDefault();
  }
  function move(event: PointerEvent) {
    if (!gesture || gesture.pointer !== event.pointerId) return;
    const at = point(event),
      g = gesture;
    if (g.kind === "draw") {
      const rect = normalizeRectangle(g.start, at, page.width, page.height);
      gesture = { ...g, rect: rect ?? { ...g.start, w: 0, h: 0 } };
    } else if (g.region) {
      const r = g.region,
        dx = at.x - g.start.x,
        dy = at.y - g.start.y;
      gesture = {
        ...g,
        rect:
          g.kind === "move"
            ? {
                x: Math.max(0, Math.min(page.width - r.w, r.x + dx)),
                y: Math.max(0, Math.min(page.height - r.h, r.y + dy)),
                w: r.w,
                h: r.h,
              }
            : {
                x: r.x,
                y: r.y,
                w: Math.max(1, Math.min(page.width - r.x, r.w + dx)),
                h: Math.max(1, Math.min(page.height - r.y, r.h + dy)),
              },
      };
    }
  }
  function finish(event: PointerEvent) {
    if (!gesture || gesture.pointer !== event.pointerId) return;
    const g = gesture;
    gesture = null;
    if (svg.hasPointerCapture(event.pointerId))
      svg.releasePointerCapture(event.pointerId);
    if (g.rect.w < 3 || g.rect.h < 3) return;
    if (g.kind === "draw") onrectangle(g.rect, mode);
    else if (
      g.region &&
      Math.abs(g.rect.x - g.region.x) +
        Math.abs(g.rect.y - g.region.y) +
        Math.abs(g.rect.w - g.region.w) +
        Math.abs(g.rect.h - g.region.h) >
        0.5
    )
      ongeometry(g.region.id, g.rect);
  }
  function low(region: OcrRegion) {
    return (
      region.review !== "approved" &&
      (region.confidenceBasis !== "recognition" ||
        region.confidence === null ||
        region.confidence < threshold)
    );
  }
</script>

<div class="canvas-shell">
  <div class="canvas-hint">
    <MousePointer2 size={13} /><span>{hint}</span><span class="scale"
      >{Math.round(scale * 100)}%</span
    >
  </div>
  <div class="viewport" bind:this={viewport}>
    <div
      class="paper"
      style:width="{width * scale}px"
      style:height="{height * scale}px"
      class:drawing={mode !== "select" && canDraw}
    >
      <img
        src={imageUrl(original ? page.originalImagePath : page.imagePath)}
        alt={original
          ? `Original scan: ${page.name}`
          : `Working page: ${page.name}`}
        draggable="false"
        onload={(e) => {
          if (original) {
            originalWidth = (e.currentTarget as HTMLImageElement).naturalWidth;
            originalHeight = (e.currentTarget as HTMLImageElement)
              .naturalHeight;
          }
        }}
      />
      {#if !original}
        <!-- Geometry editing has equivalent numeric controls in the region inspector. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <svg
          bind:this={svg}
          class="overlay"
          viewBox="0 0 {page.width} {page.height}"
          aria-label="Page regions"
          onpointerdown={beginDraw}
          onpointermove={move}
          onpointerup={finish}
          onpointercancel={() => (gesture = null)}
        >
          {#if overlays || mode !== "select"}
            {#each page.regions as region, index (region.id)}
              {@const box =
                gesture?.region?.id === region.id ? gesture.rect : region}
              <g class:excluded={region.excluded}>
                <rect
                  x={box.x}
                  y={box.y}
                  width={box.w}
                  height={box.h}
                  class="region"
                  class:low={low(region)}
                  class:approved={region.review === "approved"}
                  class:selected={selectedId === region.id}
                  role="button"
                  tabindex={mode === "select" ? 0 : -1}
                  aria-label={`Region ${index + 1}, ${region.kind}${low(region) ? ", needs review" : ""}: ${region.text.slice(0, 100) || "Empty region"}`}
                  onpointerdown={(e) => beginRegion(e, region)}
                  onclick={() => onselect(region.id)}
                  onkeydown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      onselect(region.id);
                    }
                  }}
                />
                <g
                  class="region-label"
                  transform="translate({box.x},{Math.max(
                    0,
                    box.y - 18 / scale,
                  )})"
                  pointer-events="none"
                >
                  <rect width={25 / scale} height={18 / scale} rx={3 / scale} />
                  <text x={5 / scale} y={13 / scale} font-size={11 / scale}
                    >{index + 1}</text
                  >
                </g>
              </g>
            {/each}
            {#if selected && canDraw && mode === "select"}
              {@const box =
                gesture?.region?.id === selected.id ? gesture.rect : selected}
              <rect
                class="resize-handle"
                x={box.x + box.w - 5 / scale}
                y={box.y + box.h - 5 / scale}
                width={10 / scale}
                height={10 / scale}
                role="button"
                tabindex="-1"
                aria-label="Resize selected region using the corner or the numeric geometry fields"
                onpointerdown={(e) => beginRegion(e, selected, true)}
              />
            {/if}
          {/if}
          {#if gesture?.kind === "draw"}
            <rect
              class="draft"
              x={gesture.rect.x}
              y={gesture.rect.y}
              width={gesture.rect.w}
              height={gesture.rect.h}
            />
          {/if}
        </svg>
      {/if}
      {#if page.status === "running"}<div class="recognizing">
          <ScanLine size={20} /><span>Recognizing page…</span>
        </div>{/if}
    </div>
  </div>
</div>

<style>
  .canvas-shell {
    height: 100%;
    min-height: 240px;
    display: flex;
    flex-direction: column;
    min-width: 0;
    background: #171c24;
  }
  .canvas-hint {
    height: 34px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 16px;
    color: #a0adbe;
    background: #141a22;
    border-bottom: 1px solid #29313d;
    font-size: 11px;
    flex: none;
  }
  .canvas-hint span:not(.scale) {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scale {
    margin-left: auto;
    color: #d5dfec;
    font-variant-numeric: tabular-nums;
  }
  .viewport {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 28px 36px;
    display: grid;
    align-items: start;
    justify-items: center;
    background-image: radial-gradient(#363f4b 0.7px, transparent 0.7px);
    background-size: 16px 16px;
  }
  .paper {
    position: relative;
    flex-shrink: 0;
    background: white;
    box-shadow: 0 8px 30px #0005;
    touch-action: pan-x pan-y;
    align-self: center;
  }
  .paper.drawing {
    cursor: crosshair;
    touch-action: none;
  }
  .paper img {
    width: 100%;
    height: 100%;
    display: block;
    user-select: none;
    pointer-events: none;
  }
  .overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
    touch-action: none;
  }
  .region {
    fill: #5b93e80a;
    stroke: #377ad094;
    stroke-width: 1;
    vector-effect: non-scaling-stroke;
    cursor: move;
    transition: fill 100ms;
  }
  .region.low {
    fill: #f5af3114;
    stroke: #d99639b0;
  }
  .region.approved {
    fill: #2fb78508;
    stroke: #34af8590;
  }
  .region.selected {
    fill: #4f9fff24;
    stroke: #338cff;
    stroke-width: 2;
  }
  .region:focus-visible {
    stroke: #147cfb;
    stroke-width: 3;
    outline: none;
  }
  .excluded {
    opacity: 0.3;
  }
  .excluded .region {
    stroke-dasharray: 5 4;
  }
  .region-label rect {
    fill: #25364de0;
  }
  .region-label text {
    fill: #fff;
    font-family: system-ui, sans-serif;
    letter-spacing: 0;
  }
  .draft {
    fill: #4f9fff22;
    stroke: #338cff;
    stroke-width: 2;
    stroke-dasharray: 6 4;
    vector-effect: non-scaling-stroke;
    pointer-events: none;
  }
  .resize-handle {
    fill: #fff;
    stroke: #338cff;
    stroke-width: 2;
    vector-effect: non-scaling-stroke;
    cursor: nwse-resize;
  }
  .recognizing {
    position: absolute;
    inset: 0;
    display: flex;
    gap: 10px;
    align-items: center;
    justify-content: center;
    background: #142136c9;
    color: white;
    font-size: 13px;
    backdrop-filter: blur(1px);
    pointer-events: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .region {
      transition: none;
    }
  }
</style>
