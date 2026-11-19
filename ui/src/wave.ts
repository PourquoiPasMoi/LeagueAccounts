// Shared by the music page and the tray player: the moving sound wave bars and
// the cover's main colour. No timers here; callers draw from their own frame loop.

/** Bars mirrored around the middle, rising fast and falling slowly. `shown`
 * keeps the smoothed values between frames. Returns true while still moving. */
export function drawBars(canvas: HTMLCanvasElement, shown: number[], levels: number[], tint: string, accent: string): boolean {
  const ratio = window.devicePixelRatio || 1;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  if (!width || !height) return false;
  if (canvas.width !== Math.round(width * ratio) || canvas.height !== Math.round(height * ratio)) {
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
  }
  const context = canvas.getContext("2d");
  if (!context) return false;
  let moving = false;
  for (let band = 0; band < shown.length; band++) {
    const target = (levels[band] ?? 0) / 255;
    const value = shown[band];
    shown[band] = target > value ? value + (target - value) * 0.55 : value + (target - value) * 0.12;
    if (shown[band] > 0.004) moving = true;
  }
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);
  const count = shown.length;
  const gap = width < 300 ? 2 : 3;
  const bar = Math.max(2, (width - gap * (count - 1)) / count);
  const middle = height / 2;
  const gradient = context.createLinearGradient(0, 0, width, 0);
  gradient.addColorStop(0, `rgb(${tint} / .95)`);
  gradient.addColorStop(1, `rgb(${accent} / .95)`);
  context.fillStyle = gradient;
  for (let band = 0; band < count; band++) {
    const level = Math.pow(shown[band], 1.5);
    const half = Math.max(1, level * (middle - 2));
    context.beginPath();
    context.roundRect(band * (bar + gap), middle - half, bar, half * 2, Math.min(bar / 2, 3));
    context.fill();
  }
  return moving;
}

/** The captured sound follows the player's volume: lift it back so the wave
 * keeps its size at low volume (levels are 60 dB over 0..255). */
export function compensate(levels: number[], volume: number): number[] {
  if (volume <= 0) return levels.map(() => 0);
  const lift = (20 * Math.log10(100 / Math.max(5, volume)) * 255) / 60;
  return levels.map((level) => (level > 0 ? Math.min(255, level + lift) : 0));
}

/** The cover's main colour as "r g b" (a 24×24 sample), or null when the
 * picture cannot be read. Colourful, not too dark pixels count most. */
export function coverTint(image: HTMLImageElement): string | null {
  try {
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 24;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context) return null;
    context.drawImage(image, 0, 0, 24, 24);
    const data = context.getImageData(0, 0, 24, 24).data;
    let r = 0;
    let g = 0;
    let b = 0;
    let weight = 0;
    for (let i = 0; i < data.length; i += 4) {
      const max = Math.max(data[i], data[i + 1], data[i + 2]);
      const min = Math.min(data[i], data[i + 1], data[i + 2]);
      const w = (max - min + 8) * (max > 40 ? 1 : 0.1);
      r += data[i] * w;
      g += data[i + 1] * w;
      b += data[i + 2] * w;
      weight += w;
    }
    if (!weight) return null;
    // Lifted so it glows on the dark background.
    const lift = (value: number) => Math.round(Math.min(255, (value / weight) * 1.25 + 30));
    return `${lift(r)} ${lift(g)} ${lift(b)}`;
  } catch {
    return null;
  }
}
