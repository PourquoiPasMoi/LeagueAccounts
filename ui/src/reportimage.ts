// Draws a patch notes report as one tall image, with the app's own colours and
// font (read from the page's design tokens), on a canvas. The same code lays
// the cards out (measuring) and paints them, at any pixel scale, so the live
// preview and the saved high-resolution file are the same picture.
import { gameImage, type PatchBlock, type PatchNode, type PatchSpan } from "./api";

export interface ReportSection {
  title: string;
  blocks: PatchBlock[];
}

export interface ReportOptions {
  title: string;
  subtitle: string;
  sections: ReportSection[];
  /** Cards side by side, 1 to 5. */
  columns: number;
  /** The designers' notes about each change. */
  quotes: boolean;
  /** Big pictures (skins, maps). */
  pictures: boolean;
  /** "Made with League Companion" at the bottom. */
  credit: string;
}

export interface Painted {
  canvas: HTMLCanvasElement;
  /** Size in layout units (the canvas has `scale` pixels per unit). */
  width: number;
  height: number;
  scale: number;
  /** The scale that was asked for (`scale` is lower when the canvas would be too big). */
  asked: number;
}

const PAD = 44;
const GAP = 14;
const CARD = 500;
const INSET = 20;
/// Chromium refuses canvases beyond these.
const MAX_SIDE = 16000;
const MAX_AREA = 150e6;

interface Theme {
  bg: string;
  text: string;
  text2: string;
  text3: string;
  accent: string;
  accentRgb: string;
  line: string;
  card: string;
  family: string;
}

function theme(): Theme {
  const css = getComputedStyle(document.documentElement);
  const token = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  return {
    bg: token("--bg", "#0b0b0d"),
    text: token("--text", "#f5f5f7"),
    text2: token("--text-2", "#a1a1aa"),
    text3: token("--text-3", "#7d7d86"),
    accent: token("--accent", "#0a84ff"),
    accentRgb: token("--accent-rgb", "10 132 255").replace(/\s+/g, ", "),
    line: "rgba(255, 255, 255, 0.09)",
    card: "rgba(255, 255, 255, 0.04)",
    family: token("--font-text", "Chillax, 'Segoe UI', sans-serif"),
  };
}

// ---------------------------------------------------------------- pictures

const pictures = new Map<string, Promise<HTMLImageElement | null>>();

/** A picture ready to draw (null when it cannot load). Asked with CORS so the canvas can be saved. */
function picture(source: string): Promise<HTMLImageElement | null> {
  let pending = pictures.get(source);
  if (!pending) {
    pending = new Promise((resolve) => {
      const element = new Image();
      element.crossOrigin = "anonymous";
      element.onload = () => resolve(element);
      element.onerror = () => resolve(null);
      // Its own cache entry: the page's <img> copies were stored without CORS.
      element.src = source.startsWith("/") ? source : `${gameImage(source)}?canvas=1`;
    });
    pictures.set(source, pending);
  }
  return pending;
}

function wanted(options: ReportOptions): string[] {
  const keys = new Set<string>();
  for (const section of options.sections) {
    for (const block of section.blocks) {
      if (block.icon) keys.add(block.icon);
      for (const node of block.nodes) {
        if (node.k === "h" && node.icon) keys.add(node.icon);
        if (node.k === "img" && (node.small || options.pictures)) keys.add(node.src);
      }
    }
  }
  return [...keys];
}

// ---------------------------------------------------------------- text

interface Run {
  text: string;
  weight: number;
  color: string;
  arrow?: boolean;
}

const ARROW = /\s*(?:⇒|→|=>)\s*/;
const TOKENS = /\n|\s+|[⺀-鿿가-힯＀-￯]|[^\s⺀-鿿가-힯＀-￯]+/g;
const ARROW_WIDTH = 26;

class Painter {
  private readonly widths = new Map<string, number>();
  constructor(
    readonly ctx: CanvasRenderingContext2D,
    readonly look: Theme,
    readonly images: Map<string, HTMLImageElement | null>,
    readonly options: ReportOptions,
    /** False while measuring. */
    readonly draw: boolean,
  ) {}

  font(weight: number, size: number): string {
    return `${weight} ${size}px ${this.look.family}`;
  }

  measure(text: string, weight: number, size: number): number {
    const key = `${weight}|${size}|${text}`;
    let width = this.widths.get(key);
    if (width === undefined) {
      this.ctx.font = this.font(weight, size);
      width = this.ctx.measureText(text).width;
      this.widths.set(key, width);
    }
    return width;
  }

  /** Wrapped text in mixed looks; returns the height used. */
  text(runs: Run[], x: number, y: number, width: number, size: number, leading = 1.5): number {
    const lineHeight = Math.round(size * leading);
    let cursor = 0;
    let line = 0;
    let space = 0;
    const put = (run: Run, text: string, wide: number) => {
      if (cursor > 0 && cursor + space + wide > width) {
        cursor = 0;
        line += 1;
        space = 0;
      }
      cursor += cursor > 0 ? space : 0;
      space = 0;
      if (this.draw) {
        const baseline = y + line * lineHeight + lineHeight / 2;
        if (run.arrow) {
          this.arrow(x + cursor + 6, baseline, run.color);
        } else {
          this.ctx.font = this.font(run.weight, size);
          this.ctx.fillStyle = run.color;
          this.ctx.textBaseline = "middle";
          this.ctx.fillText(text, x + cursor, baseline + size * 0.04);
        }
      }
      cursor += wide;
    };
    for (const run of runs) {
      if (run.arrow) {
        put(run, "", ARROW_WIDTH);
        space = 0;
        continue;
      }
      for (const token of run.text.match(TOKENS) ?? []) {
        if (token === "\n") {
          cursor = 0;
          line += 1;
          space = 0;
        } else if (/^\s+$/.test(token)) {
          space = Math.max(space, this.measure(" ", run.weight, size));
        } else {
          let rest = token;
          const wide = (text: string) => this.measure(text, run.weight, size);
          const newLine = () => {
            cursor = 0;
            line += 1;
            space = 0;
          };
          while (rest) {
            const room = width - (cursor > 0 ? cursor + space : 0);
            if (wide(rest) <= room || (cursor === 0 && rest.length === 1)) {
              put(run, rest, wide(rest));
              break;
            }
            // It fits on a line of its own.
            if (cursor > 0 && wide(rest) <= width) {
              newLine();
              continue;
            }
            // A word wider than a whole line is cut where it stops fitting.
            let fit = 0;
            while (fit < rest.length && wide(rest.slice(0, fit + 1)) <= room) fit += 1;
            if (fit === 0) {
              if (cursor > 0) {
                newLine();
                continue;
              }
              fit = 1;
            }
            put(run, rest.slice(0, fit), wide(rest.slice(0, fit)));
            rest = rest.slice(fit);
            newLine();
          }
        }
      }
    }
    return (line + (cursor > 0 || line === 0 ? 1 : 0)) * lineHeight;
  }

  arrow(x: number, y: number, color: string): void {
    const { ctx } = this;
    ctx.save();
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.7;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();
    ctx.moveTo(x, y);
    ctx.lineTo(x + 13, y);
    ctx.moveTo(x + 9, y - 4);
    ctx.lineTo(x + 13, y);
    ctx.lineTo(x + 9, y + 4);
    ctx.stroke();
    ctx.restore();
  }

  round(x: number, y: number, width: number, height: number, radius: number): void {
    this.ctx.beginPath();
    this.ctx.roundRect(x, y, width, height, radius);
  }

  /** A picture cropped to fill a rounded box. */
  image(key: string, x: number, y: number, width: number, height: number, radius: number): void {
    const element = this.images.get(key);
    if (!this.draw || !element) return;
    const { ctx } = this;
    const ratio = Math.max(width / element.naturalWidth, height / element.naturalHeight);
    const sourceWidth = width / ratio;
    const sourceHeight = height / ratio;
    ctx.save();
    this.round(x, y, width, height, radius);
    ctx.clip();
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(element, (element.naturalWidth - sourceWidth) / 2, (element.naturalHeight - sourceHeight) / 2, sourceWidth, sourceHeight, x, y, width, height);
    ctx.restore();
    ctx.save();
    this.round(x + 0.5, y + 0.5, width - 1, height - 1, radius);
    ctx.strokeStyle = "rgba(255, 255, 255, 0.14)";
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.restore();
  }

  plain(spans: PatchSpan[], color: string, weight = 400): Run[] {
    return spans.map((span) => ({ text: span.t, weight: span.b ? 600 : weight, color: span.b ? this.look.text : span.href ? this.look.accent : color }));
  }

  /** "Damage: 10 ⇒ 20": the stat's name bright, the old value dim, an arrow, the new value bright. */
  change(spans: PatchSpan[]): Run[] {
    const { look } = this;
    if (!spans.some((span) => ARROW.test(span.t))) return this.plain(spans, look.text2);
    const runs: Run[] = [];
    let after = false;
    for (const span of spans) {
      span.t.split(ARROW).forEach((part, index) => {
        if (index > 0) {
          after = true;
          runs.push({ text: "", weight: 400, color: look.accent, arrow: true });
        }
        if (!part) return;
        runs.push(after ? { text: part, weight: 600, color: look.text } : span.b ? { text: part, weight: 600, color: look.text } : { text: part, weight: 400, color: look.text3 });
      });
    }
    return runs;
  }

  /** The nodes that will be shown, with rules tidied after leaving some out. */
  nodes(block: PatchBlock): PatchNode[] {
    const kept = block.nodes.filter((node) => {
      if (node.k === "q") return this.options.quotes;
      if (node.k === "img") return node.small ? true : this.options.pictures && Boolean(this.images.get(node.src));
      return true;
    });
    const out: PatchNode[] = [];
    for (const node of kept) {
      if (node.k === "hr" && (!out.length || out[out.length - 1].k === "hr")) continue;
      out.push(node);
    }
    while (out.length && out[out.length - 1].k === "hr") out.pop();
    return out;
  }

  /** One card at (x, y); returns its height. */
  card(block: PatchBlock, x: number, y: number, width: number, height = 0): number {
    const { ctx, look } = this;
    if (this.draw) {
      this.round(x, y, width, height, 16);
      ctx.fillStyle = look.card;
      ctx.fill();
      this.round(x + 0.5, y + 0.5, width - 1, height - 1, 16);
      ctx.strokeStyle = look.line;
      ctx.lineWidth = 1;
      ctx.stroke();
    }
    const left = x + INSET;
    const inner = width - INSET * 2;
    let top = y + INSET;
    if (block.title || block.icon) {
      const icon = block.icon && this.images.get(block.icon) ? 44 : 0;
      if (icon) this.image(block.icon, left, top, 44, 44, 12);
      const titleLeft = icon ? left + 56 : left;
      const titleHeight = block.title ? this.textHeight([{ text: block.title, weight: 700, color: look.text }], titleLeft, top, inner - (titleLeft - left), 19, 1.25, icon) : 0;
      top += Math.max(icon, titleHeight) + 12;
    }
    let first = true;
    for (const node of this.nodes(block)) {
      if (node.k === "h") {
        top += first ? 0 : 8;
        const icon = node.icon && this.images.get(node.icon) ? 26 : 0;
        if (icon) this.image(node.icon, left, top, 26, 26, 7);
        const headLeft = icon ? left + 35 : left;
        const used = this.textHeight(this.plain(node.spans.map((span) => ({ ...span, b: true })), look.text, 600), headLeft, top, inner - (headLeft - left), 14.5, 1.3, icon);
        top += Math.max(icon, used) + 6;
      } else if (node.k === "p") {
        top += this.text(this.plain(node.spans, look.text2), left, top, inner, 13.5) + 6;
      } else if (node.k === "q") {
        const used = this.text(this.plain(node.spans, look.text3), left + 14, top, inner - 14, 13, 1.5);
        if (this.draw) {
          ctx.fillStyle = "rgba(255, 255, 255, 0.16)";
          ctx.fillRect(left, top + 2, 2, used - 4);
        }
        top += used + 8;
      } else if (node.k === "li") {
        const indent = 8 + Math.min(node.depth, 3) * 18;
        top += this.text(this.change(node.spans), left + indent, top, inner - indent, 13.5) + 5;
      } else if (node.k === "hr") {
        if (this.draw) {
          ctx.fillStyle = look.line;
          ctx.fillRect(left, top + 5, inner, 1);
        }
        top += 14;
      } else if (node.small) {
        this.image(node.src, left, top, 34, 34, 8);
        top += 40;
      } else {
        const element = this.images.get(node.src);
        if (element) {
          const tall = Math.min(Math.round((inner * element.naturalHeight) / element.naturalWidth), 320);
          this.image(node.src, left, top, inner, tall, 10);
          top += tall + 10;
        }
      }
      first = false;
    }
    return Math.max(top - y + INSET - 5, INSET * 2 + 10);
  }

  /** Text beside an icon: centred on the icon when it is one line. */
  private textHeight(runs: Run[], x: number, y: number, width: number, size: number, leading: number, icon: number): number {
    const quiet = new Painter(this.ctx, this.look, this.images, this.options, false);
    const used = quiet.text(runs, x, y, width, size, leading);
    if (this.draw) this.text(runs, x, y + Math.max(0, (icon - used) / 2), width, size, leading);
    return used;
  }
}

/** Width of the image in layout units for a number of columns. */
export const reportWidth = (columns: number): number => PAD * 2 + columns * CARD + (columns - 1) * GAP;

/** A section's heading or one card, with its place once laid out. */
interface Item {
  head?: string;
  block?: PatchBlock;
  x: number;
  y: number;
  height: number;
}

/// A card with more lines than this is cut at its rules, so it can continue in the next column.
const LONG_CARD = 26;

/** A very long card as several: cut at its rules (the first piece keeps the name and portrait). */
function pieces(block: PatchBlock): PatchBlock[] {
  if (block.nodes.length <= LONG_CARD) return [block];
  const out: PatchBlock[] = [];
  let current: PatchBlock = { ...block, nodes: [] };
  for (const node of block.nodes) {
    if (node.k === "hr" && current.nodes.length >= 6) {
      out.push(current);
      current = { title: "", icon: "", nodes: [] };
    } else {
      current.nodes.push(node);
    }
  }
  if (current.nodes.length || !out.length) out.push(current);
  return out;
}

/**
 * Headings and cards flow down the first column, then the next, like a
 * newspaper: sections stay in order and no column is left half empty.
 * Returns the height of the tallest column.
 */
function flow(items: Item[], columns: number, top: number): number {
  const total = items.reduce((sum, item) => sum + item.height + GAP, 0);
  const tallest = items.reduce((most, item) => Math.max(most, item.height), 0);
  const place = (target: number): number => {
    let column = 0;
    let y = 0;
    let bottom = 0;
    items.forEach((item, index) => {
      // A heading takes its first card along: it never ends a column.
      const needed = item.head !== undefined && items[index + 1] ? item.height + GAP + items[index + 1].height : item.height;
      if (y > 0 && y + needed > target && column < columns - 1) {
        column += 1;
        y = 0;
      }
      // No gap above a heading that starts a column.
      const lift = item.head !== undefined && y === 0 ? 20 : 0;
      item.x = PAD + column * (CARD + GAP);
      item.y = top + y - lift;
      y += item.height - lift + GAP;
      bottom = Math.max(bottom, y - GAP);
    });
    return bottom;
  };
  // The even share rarely fits exactly: try a little more room until the last column stops overflowing.
  let best = Number.POSITIVE_INFINITY;
  let chosen = Math.max(total / columns, tallest);
  for (let target = chosen, tries = 0; tries < 24; tries += 1, target *= 1.03) {
    const bottom = place(target);
    if (bottom < best) {
      best = bottom;
      chosen = target;
    }
  }
  return place(chosen);
}

function paint(painter: Painter, items: Item[]): number {
  const { ctx, look, options, draw } = painter;
  const columns = Math.max(1, Math.min(5, options.columns));
  const width = reportWidth(columns);
  let top = PAD;
  // Head: the app's mark, the title, a line about it.
  const mark = painter.images.get("/icon.png");
  if (mark && draw) ctx.drawImage(mark, PAD, top + 2, 46, 46);
  const titleLeft = mark ? PAD + 62 : PAD;
  top += painter.text([{ text: options.title || " ", weight: 700, color: look.text }], titleLeft, top, width - PAD - titleLeft, columns > 1 ? 40 : 32, 1.2);
  if (options.subtitle) top += 4 + painter.text([{ text: options.subtitle, weight: 400, color: look.text2 }], titleLeft, top + 4, width - PAD - titleLeft, 15.5, 1.45);
  top += 22;
  if (draw) {
    ctx.fillStyle = look.line;
    ctx.fillRect(PAD, top, width - PAD * 2, 1);
  }
  top += 26;
  if (!draw) {
    for (const item of items) {
      item.height = item.block ? painter.card(item.block, 0, 0, CARD) : 20 + painter.text([{ text: item.head ?? "", weight: 700, color: look.text }], 0, 0, CARD, 24, 1.25);
    }
  }
  const bottom = flow(items, columns, top);
  if (draw) {
    for (const item of items) {
      if (item.block) painter.card(item.block, item.x, item.y, CARD, item.height);
      else painter.text([{ text: item.head ?? "", weight: 700, color: look.text }], item.x, item.y + 20, CARD, 24, 1.25);
    }
  }
  top += bottom + 26;
  if (options.credit) {
    const wide = painter.measure(options.credit, 500, 13);
    painter.text([{ text: options.credit, weight: 500, color: look.text3 }], width - PAD - wide - 1, top, wide + 2, 13, 1.4);
    top += 20;
  }
  return top + PAD - 14;
}

/** The largest scale a canvas of this size can have. */
function allowed(scale: number, width: number, height: number): number {
  return Math.max(0.25, Math.min(scale, MAX_SIDE / height, MAX_SIDE / width, Math.sqrt(MAX_AREA / (width * height))));
}

/** Draw the report on `canvas` (or a new one) with `scale` pixels per layout unit. */
export async function paintReport(options: ReportOptions, scale: number, canvas = document.createElement("canvas")): Promise<Painted> {
  const look = theme();
  await Promise.all([400, 500, 600, 700].map((weight) => document.fonts.load(`${weight} 16px ${look.family}`).catch(() => [])));
  const keys = ["/icon.png", ...wanted(options)];
  const loaded = await Promise.all(keys.map((key) => picture(key)));
  const images = new Map(keys.map((key, index) => [key, loaded[index]]));
  const scratch = document.createElement("canvas").getContext("2d")!;
  const items: Item[] = [];
  for (const section of options.sections) {
    if (!section.blocks.length) continue;
    if (section.title) items.push({ head: section.title, x: 0, y: 0, height: 0 });
    for (const block of section.blocks) for (const piece of pieces(block)) items.push({ block: piece, x: 0, y: 0, height: 0 });
  }
  const width = reportWidth(Math.max(1, Math.min(5, options.columns)));
  const height = Math.ceil(paint(new Painter(scratch, look, images, options, false), items));
  const real = allowed(scale, width, height);
  canvas.width = Math.round(width * real);
  canvas.height = Math.round(height * real);
  const ctx = canvas.getContext("2d")!;
  ctx.setTransform(real, 0, 0, real, 0, 0);
  ctx.fillStyle = look.bg;
  ctx.fillRect(0, 0, width, height);
  // The app's soft light in the top right corner.
  const glow = ctx.createRadialGradient(width * 0.82, -80, 0, width * 0.82, -80, Math.max(620, width * 0.7));
  glow.addColorStop(0, `rgba(${look.accentRgb}, 0.2)`);
  glow.addColorStop(1, `rgba(${look.accentRgb}, 0)`);
  ctx.fillStyle = glow;
  ctx.fillRect(0, 0, width, Math.min(height, 900));
  paint(new Painter(ctx, look, images, options, true), items);
  return { canvas, width, height, scale: real, asked: scale };
}

export interface Exported {
  blob: Blob;
  extension: "png" | "jpg";
  /** Pixels of the file. */
  width: number;
  height: number;
  /** The image is so tall that it had to be made less sharp than asked: more columns help. */
  tall: boolean;
}

const encode = (canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> => new Promise((resolve) => canvas.toBlob(resolve, type, quality));

/**
 * The report as a file of at most `maxBytes`: PNG when it fits (sharpest
 * text), else the best JPEG that fits, else the same at a lower resolution.
 * `alive()` false = the options changed meanwhile: stop (returns null).
 */
export async function exportReport(options: ReportOptions, scale: number, maxBytes: number, alive: () => boolean = () => true): Promise<Exported | null> {
  let wantedScale = scale;
  for (let attempt = 0; attempt < 8; attempt += 1) {
    const painted = await paintReport(options, wantedScale);
    if (!alive()) return null;
    const done = (blob: Blob, extension: "png" | "jpg"): Exported => ({ blob, extension, width: painted.canvas.width, height: painted.canvas.height, tall: attempt === 0 && painted.scale < scale * 0.98 });
    const png = await encode(painted.canvas, "image/png");
    if (!alive()) return null;
    if (png && png.size <= maxBytes) return done(png, "png");
    let low = 0.6;
    let high = 0.95;
    let best: Blob | null = null;
    const top = await encode(painted.canvas, "image/jpeg", high);
    if (!alive()) return null;
    if (top && top.size <= maxBytes) return done(top, "jpg");
    for (let step = 0; step < 5; step += 1) {
      const middle = (low + high) / 2;
      const blob = await encode(painted.canvas, "image/jpeg", middle);
      if (!alive()) return null;
      if (blob && blob.size <= maxBytes) {
        best = blob;
        low = middle;
      } else {
        high = middle;
      }
    }
    if (!best) {
      const floor = await encode(painted.canvas, "image/jpeg", 0.6);
      if (floor && floor.size <= maxBytes) best = floor;
    }
    if (best) return done(best, "jpg");
    // Still too big: fewer pixels. The canvas may already be smaller than asked (its size limit).
    wantedScale = painted.scale * 0.82;
    if (wantedScale < 0.3) return png ? done(png, "png") : null;
  }
  return null;
}
