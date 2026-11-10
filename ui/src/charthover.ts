// Live readout for the SVG charts: a guide line and a tooltip follow the
// pointer and show the values of the nearest point, like online stock charts.
//
// A chart opts in with `data-points` (JSON of HoverPoint, in SVG units) and an
// empty `<g class="chart-hover">` that this module fills with the guide.

export interface HoverPoint {
  x: number;
  y: number;
  title: string;
  lines: string[];
  /** Colors the tooltip's accent: a win, a loss, or a tier color. */
  color?: string;
}

const cache = new WeakMap<SVGSVGElement, HoverPoint[]>();

function pointsOf(svg: SVGSVGElement): HoverPoint[] {
  let points = cache.get(svg);
  if (!points) {
    try {
      points = JSON.parse(svg.dataset.points ?? "[]") as HoverPoint[];
    } catch {
      points = [];
    }
    cache.set(svg, points);
  }
  return points;
}

let tooltip: HTMLDivElement | null = null;
let active: SVGSVGElement | null = null;

function hide(): void {
  active?.querySelector(".chart-hover")?.setAttribute("visibility", "hidden");
  active = null;
  tooltip?.remove();
  tooltip = null;
}

function show(svg: SVGSVGElement, clientX: number): void {
  const points = pointsOf(svg);
  const matrix = svg.getScreenCTM();
  if (!points.length || !matrix) return hide();
  const x = (clientX - matrix.e) / matrix.a;
  const point = points.reduce((best, candidate) => (Math.abs(candidate.x - x) < Math.abs(best.x - x) ? candidate : best));
  if (active && active !== svg) hide();
  active = svg;

  const box = svg.viewBox.baseVal;
  const guide = svg.querySelector<SVGGElement>(".chart-hover");
  if (guide) {
    guide.setAttribute("visibility", "visible");
    guide.innerHTML = `
      <line x1="${point.x}" x2="${point.x}" y1="${box.y + 6}" y2="${box.y + box.height - 20}" class="hover-guide"/>
      <circle cx="${point.x}" cy="${point.y}" r="5.5" class="hover-dot"${point.color ? ` style="fill:${point.color}"` : ""}/>`;
  }

  if (!tooltip) {
    tooltip = document.createElement("div");
    tooltip.className = "chart-tooltip";
    tooltip.setAttribute("role", "status");
    document.body.append(tooltip);
  }
  tooltip.replaceChildren();
  const title = document.createElement("b");
  title.textContent = point.title;
  tooltip.append(title);
  for (const text of point.lines) {
    const line = document.createElement("span");
    line.textContent = text;
    tooltip.append(line);
  }
  tooltip.style.setProperty("--accent", point.color ?? "#c8aa6e");

  // Anchor beside the point, flipping to the left near the right edge.
  const screenX = matrix.a * point.x + matrix.e;
  const screenY = matrix.d * point.y + matrix.f;
  const bounds = tooltip.getBoundingClientRect();
  const left = screenX + 16 + bounds.width > innerWidth - 8 ? screenX - 16 - bounds.width : screenX + 16;
  const top = Math.max(8, Math.min(screenY - bounds.height / 2, innerHeight - bounds.height - 8));
  tooltip.style.left = `${Math.max(8, left)}px`;
  tooltip.style.top = `${top}px`;
}

/** Follow the pointer over every opted-in chart inside `root`. */
export function wireChartHover(root: HTMLElement): void {
  root.addEventListener("pointermove", (event) => {
    const svg = (event.target as Element).closest?.("svg[data-points]") as SVGSVGElement | null;
    if (svg) show(svg, event.clientX);
    else if (active) hide();
  });
  root.addEventListener("pointerleave", hide);
  root.addEventListener("scroll", hide, true);
  // A re-render replaces the chart under the pointer.
  new MutationObserver(() => {
    if (active && !active.isConnected) hide();
  }).observe(root, { childList: true, subtree: true });
}

export const hideChartHover = hide;

/** `data-points` attribute value: JSON escaped for an HTML attribute. */
export function pointsAttribute(points: HoverPoint[]): string {
  return JSON.stringify(points).replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
