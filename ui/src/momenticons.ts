// Key moment glyphs for the recordings timeline: one recognizable shape per
// kind, drawn for this app on a 16×16 grid in the client's line style.

const STROKE = 'fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"';

const GLYPHS: Record<string, string> = {
  // Sword pointing up-right with a cross guard.
  kill: `<path ${STROKE} d="M13.5 2.5 6.8 9.2M10 2.5h3.5V6M4.6 7.8l3.6 3.6M5.4 10.6l-2.9 2.9"/>`,
  // Two crossed swords.
  multikill: `<path ${STROKE} d="M2.5 2.5 9.4 9.4M13.5 2.5 6.6 9.4M4.7 8.9l2.4 2.4M11.3 8.9l-2.4 2.4M5.6 10.4 3 13M10.4 10.4 13 13"/>`,
  // Skull.
  death: `<path ${STROKE} d="M8 2.2a5 5 0 0 0-5 5c0 1.6.8 2.8 2 3.5v2.1h6v-2.1c1.2-.7 2-1.9 2-3.5a5 5 0 0 0-5-5zM7 12.8v-1.6M9 12.8v-1.6"/><circle cx="6.1" cy="7.3" r="1.15" fill="currentColor"/><circle cx="9.9" cy="7.3" r="1.15" fill="currentColor"/>`,
  // Shield with a plus.
  assist: `<path ${STROKE} d="M8 2 13 4v3.8c0 2.9-2.1 5-5 6.2-2.9-1.2-5-3.3-5-6.2V4zM8 5.6v4.6M5.7 7.9h4.6"/>`,
  // Blood drop.
  firstBlood: `<path fill="currentColor" d="M8 1.8S3.6 7 3.6 9.8a4.4 4.4 0 0 0 8.8 0C12.4 7 8 1.8 8 1.8z"/><path fill="#010a13" opacity=".45" d="M6 10.2a2 2 0 0 0 2 2 .6.6 0 0 0 0-1.2.8.8 0 0 1-.8-.8.6.6 0 0 0-1.2 0z"/>`,
  // Dragon wing.
  dragon: `<path fill="currentColor" d="M1.8 13.2c1.4-5 5.4-9.4 12.4-10.6-1.7 2.2-1.9 4.3-1 6.5-2.6-.6-4.6.3-5.8 2.9-1-1.3-3-1.7-5.6 1.2z"/>`,
  // Horned head (Baron Nashor).
  baron: `<path ${STROKE} d="M3 2.5 4.6 6.3M13 2.5l-1.6 3.8M8 5.2a4 4 0 0 1 4 4v3.6H4V9.2a4 4 0 0 1 4-4zM6.4 12.8v-1.4M9.6 12.8v-1.4"/><circle cx="6.4" cy="8.9" r="1" fill="currentColor"/><circle cx="9.6" cy="8.9" r="1" fill="currentColor"/>`,
  // Eye (Rift Herald).
  herald: `<path ${STROKE} d="M1.5 8S4 3.6 8 3.6 14.5 8 14.5 8 12 12.4 8 12.4 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2.1" fill="currentColor"/>`,
  // Grub.
  grubs: `<ellipse ${STROKE} cx="8" cy="9.4" rx="3.3" ry="3.9"/><path ${STROKE} d="M6.6 5.7 5 2.7M9.4 5.7 11 2.7M4.7 8.4 2.6 7.6M4.7 10.8l-2.1.9M11.3 8.4l2.1-.8M11.3 10.8l2.1.9M8 6v6.8"/>`,
  // Turret.
  tower: `<path ${STROKE} d="M4.5 14h7M6.1 14l.6-6.8h2.6l.6 6.8M4.3 7.2h7.4L10.6 4H5.4zM8 4V1.8"/>`,
  // Inhibitor crystal.
  inhibitor: `<path ${STROKE} d="M8 1.6 12.6 8 8 14.4 3.4 8zM3.4 8h9.2M8 1.6v12.8"/>`,
  // Crown (ace: the whole enemy team is down).
  ace: `<path ${STROKE} d="M2.5 12.6h11M3 12.6 1.8 5.4l3.7 2.7L8 3l2.5 5.1 3.7-2.7-1.2 7.2z"/>`,
  // Flag (game end).
  gameEnd: `<path ${STROKE} d="M3.5 14.2V2M3.5 2.6h8.6l-2 3 2 3H3.5"/>`,
};

export function momentIcon(kind: string, size = 14): string {
  const glyph = GLYPHS[kind] ?? GLYPHS.gameEnd;
  return `<svg class="moment-icon" viewBox="0 0 16 16" width="${size}" height="${size}" aria-hidden="true">${glyph}</svg>`;
}
