<script lang="ts">
  // Over the picture: the detected blocks (numbered in reading order, outlined by
  // kind -- and dashed or dotted too, so the kind is not told by colour alone) and
  // the selection with its corner handles, and the pointer gestures that make them:
  // drag to draw a box, drag inside it to move it, drag a corner to reshape it,
  // click a block to select it, click elsewhere to clear. With the erase brush on, a
  // drag paints out what should not be read instead. Everything is kept in view
  // pixels by the engine; this draws it where the picture's placement puts it.
  import { dispatch, type BlocksSlice, type Crop, type Selection, type Stroke } from "./lib/engine";
  import { brushInView, farEnough } from "./lib/erase";
  import {
    blockAt,
    boxBetween,
    clampPoint,
    contains,
    moved,
    outline,
    role,
    toCanvas,
    toView,
    type Placement,
  } from "./lib/selection";

  let {
    placement,
    view,
    dpr,
    selection,
    blocks,
    brush,
    onerase,
    onnotice,
  }: {
    placement: Placement | null;
    view: [number, number] | null;
    dpr: number;
    selection: Selection | null;
    blocks: BlocksSlice | null;
    /** The erase brush's radius in CSS pixels while it is on, else null. */
    brush: number | null;
    /** A brush stroke finished, in view pixels. */
    onerase: (stroke: Stroke) => Promise<void>;
    onnotice: (text: string) => void;
  } = $props();

  let svg: SVGSVGElement;
  // A gesture in progress, and what it shows until the engine has it.
  type Gesture =
    | { kind: "draw"; from: [number, number]; to: [number, number] }
    | { kind: "move"; from: [number, number]; box: Crop; shown: Crop }
    | { kind: "corner"; corner: number }
    | { kind: "erase"; stroke: Stroke };
  let gesture = $state<Gesture | null>(null);
  // Where the pointer is over the picture (CSS pixels), for the brush's outline.
  let hover = $state<[number, number] | null>(null);

  const HANDLE = 10; // CSS pixels around a corner that take it

  function at(e: PointerEvent): [number, number] | null {
    if (!placement || !view) return null;
    const r = svg.getBoundingClientRect();
    return clampPoint(toView(placement, dpr, [e.clientX - r.left, e.clientY - r.top]), view);
  }

  function screen(p: [number, number]): [number, number] {
    return placement ? toCanvas(placement, dpr, p) : [0, 0];
  }

  function points(q: [number, number][]): string {
    return q.map((p) => screen(p).join(",")).join(" ");
  }

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    const p = at(e);
    if (!p) return;
    svg.setPointerCapture(e.pointerId);
    if (brush !== null && placement) {
      gesture = { kind: "erase", stroke: { points: [p], radius: brushInView(brush, placement, dpr) } };
      return;
    }
    if (selection) {
      const r = svg.getBoundingClientRect();
      const corner = outline(selection).findIndex((c) => {
        const [x, y] = screen(c);
        return Math.hypot(x - (e.clientX - r.left), y - (e.clientY - r.top)) <= HANDLE;
      });
      if (corner >= 0) {
        gesture = { kind: "corner", corner };
        return;
      }
      if (contains(selection.rect, p)) {
        gesture = { kind: "move", from: p, box: selection.rect, shown: selection.rect };
        return;
      }
    }
    gesture = { kind: "draw", from: p, to: p };
  }

  function move(e: PointerEvent) {
    const r = svg.getBoundingClientRect();
    hover = [e.clientX - r.left, e.clientY - r.top];
    const p = at(e);
    if (!p || !gesture || !view) return;
    if (gesture.kind === "erase") {
      const s = gesture.stroke;
      if (farEnough(s.points[s.points.length - 1]!, p, s.radius))
        gesture = { kind: "erase", stroke: { ...s, points: [...s.points, p] } };
    } else if (gesture.kind === "draw") gesture = { ...gesture, to: p };
    else if (gesture.kind === "move")
      gesture = { ...gesture, shown: moved(gesture.box, p[0] - gesture.from[0], p[1] - gesture.from[1], view) };
    else dispatch({ type: "move-corner", corner: gesture.corner, to: p }).catch(() => {});
  }

  async function up(e: PointerEvent) {
    const g = gesture;
    gesture = null;
    if (!g) return;
    const p = at(e) ?? (g.kind === "draw" ? g.to : [0, 0]);
    try {
      if (g.kind === "erase") {
        await onerase(g.stroke);
      } else if (g.kind === "draw") {
        const box = boxBetween(g.from, p);
        if (box) {
          await dispatch({ type: "set-selection", selection: box });
        } else {
          // A click: the block under it, or nothing.
          const i = blocks ? blockAt(blocks.blocks, g.from) : null;
          if (i !== null) await dispatch({ type: "select-block", index: i });
          else await dispatch({ type: "set-selection", selection: null });
        }
      } else if (g.kind === "move") {
        await dispatch({ type: "set-selection", selection: g.shown });
      }
    } catch (err) {
      onnotice(String(err));
    }
  }

  const drawing = $derived(
    gesture?.kind === "draw" ? boxBetween(gesture.from, gesture.to) : gesture?.kind === "move" ? gesture.shown : null,
  );
  const shownSelection = $derived<Selection | null>(
    drawing ? { rect: drawing, quad: null } : selection,
  );
  // The stroke being painted, until the engine has painted it.
  const painting = $derived(gesture?.kind === "erase" ? gesture.stroke : null);
</script>

<!-- The gestures have keyboard equivalents (the shortcuts: next block, select,
     clear); this layer is for the pointer. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<svg
  bind:this={svg}
  class="overlay"
  class:drawing={gesture !== null && brush === null}
  class:erasing={brush !== null}
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={() => (gesture = null)}
  onpointerleave={() => (hover = null)}
  aria-hidden="true"
>
  {#if placement && blocks?.enabled}
    {#each blocks.blocks as b, i}
      {@const q = b.quad ?? outline({ rect: b.rect, quad: null })}
      {@const [x, y] = screen(q[0])}
      <g class="block {role(b.label)}" class:selected={blocks.selected === i}>
        <polygon class="under" points={points(q)} />
        <polygon class="line" points={points(q)} />
        <rect class="tag" x={x} y={y} width="1.6em" height="1.3em" />
        <text x={x + 4} y={y} dy="1em">{i + 1}</text>
      </g>
    {/each}
  {/if}
  {#if placement && shownSelection}
    {@const q = outline(shownSelection)}
    <polygon class="selection-under" points={points(q)} />
    <polygon class="selection" points={points(q)} />
    {#if !drawing && brush === null}
      {#each q as c}
        {@const [x, y] = screen(c)}
        <circle class="handle" cx={x} cy={y} r="6" />
      {/each}
    {/if}
  {/if}
  {#if placement && painting}
    {@const width = 2 * painting.radius * placement.scale / dpr}
    <polyline class="paint-under" points={points(painting.points)} stroke-width={width + 3} />
    <polyline class="paint" points={points(painting.points)} stroke-width={width} />
  {/if}
  {#if brush !== null && hover}
    <circle class="brush" cx={hover[0]} cy={hover[1]} r={brush} />
  {/if}
</svg>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    cursor: crosshair;
    touch-action: none;
  }
  .overlay.drawing {
    cursor: grabbing;
  }
  .overlay.erasing {
    cursor: none;
  }
  polyline {
    fill: none;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .paint-under {
    stroke: rgb(0 0 0 / 0.6);
  }
  .paint {
    stroke: rgb(255 255 255 / 0.85);
  }
  .brush {
    fill: none;
    stroke: var(--line, #ffd400);
    stroke-width: 2;
    filter: drop-shadow(0 0 1px #000);
  }
  polygon {
    fill: none;
  }
  .under,
  .selection-under {
    stroke: rgb(0 0 0 / 0.8);
    stroke-width: 5;
  }
  .line {
    stroke-width: 2.5;
  }
  /* Kind by colour and by line: text solid, formula dashed, figure dotted. */
  .text .line {
    stroke: #ff5a28;
  }
  .formula .line {
    stroke: #c85aff;
    stroke-dasharray: 8 4;
  }
  .figure .line {
    stroke: #3cbeff;
    stroke-dasharray: 2 4;
  }
  .other .line {
    stroke: #aaa;
    stroke-dasharray: 1 6;
  }
  .tag {
    fill: rgb(0 0 0 / 0.75);
  }
  text {
    fill: #fff;
    font: bold 0.9rem system-ui, sans-serif;
  }
  .selected .line {
    stroke: var(--line, #ffd400);
    stroke-width: 3.5;
  }
  .selection {
    stroke: var(--line, #ffd400);
    stroke-width: 3;
  }
  .handle {
    fill: var(--line, #ffd400);
    stroke: #000;
    stroke-width: 1.5;
  }
</style>
