// Small interaction effects. One delegated listener each, nothing runs while idle.

const SPOT = ".card, .side-tile, .rec-card, .ranked-card, .stats-view .profile-panel";

/** A soft light that follows the pointer over cards (CSS reads --mx / --my). */
function pointerLight(): void {
  let frame = 0;
  let last: PointerEvent | null = null;
  document.addEventListener("pointermove", event => {
    last = event;
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      const target = last?.target instanceof Element ? last.target.closest<HTMLElement>(SPOT) : null;
      if (!target || !last) return;
      const box = target.getBoundingClientRect();
      target.style.setProperty("--mx", `${last.clientX - box.left}px`);
      target.style.setProperty("--my", `${last.clientY - box.top}px`);
    });
  }, { passive: true });
}

/** The scrolled state of the main page: the toolbar gets its glass bar only once content slides under it. */
function scrollState(): void {
  const main = document.querySelector<HTMLElement>(".main");
  if (!main) return;
  const update = () => main.classList.toggle("is-scrolled", main.scrollTop > 8);
  main.addEventListener("scroll", update, { passive: true });
  update();
}

export function wireFx(): void {
  if (!matchMedia("(prefers-reduced-motion: reduce)").matches) pointerLight();
  scrollState();
}
