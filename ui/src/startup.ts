import { api } from "./api";

/** The opening is given this long from the moment the page starts (ms), so it never cuts off mid-movement. */
const OPENING_MS = 1700;
/** The loading screen's way out (matches `startup-out` in startup.css). */
const LEAVING_MS = 600;

/** Decode the first screen, including CSS backgrounds and icon masks, offscreen. */
export async function revealApp(): Promise<void> {
  const images = [...document.images].map(image => image.decode().catch(() => {}));
  const urls = new Set<string>();
  for (const element of document.body.querySelectorAll("*")) {
    if (element.closest("#startup-loader")) continue;
    for (const pseudo of [null, "::before", "::after"]) {
      const style = getComputedStyle(element, pseudo);
      for (const value of [style.backgroundImage, style.maskImage]) {
        for (const match of value.matchAll(/url\(["']?(.*?)["']?\)/g)) urls.add(match[1]);
      }
    }
  }
  for (const src of urls) {
    const image = new Image();
    image.src = src;
    images.push(image.decode().catch(() => {}));
  }
  // Reading layout starts font requests even though the interface is hidden.
  void document.body.offsetHeight;
  await Promise.all([document.fonts.ready, ...images]);
  await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  document.querySelectorAll(".card.enter, .card.enter-stagger").forEach(card => card.classList.remove("enter", "enter-stagger"));
  // The app works from here on: say so before the opening finishes playing.
  void api.appReady().catch(error => console.error("Update health could not be recorded", error));
  const loader = document.querySelector<HTMLElement>("#startup-loader");
  const calm = matchMedia("(prefers-reduced-motion: reduce)").matches;
  // No waiting when nobody watches (started hidden in the tray) or after the update screen.
  const watched = document.visibilityState === "visible" && !loader?.querySelector(".update-screen");
  if (loader && watched && !calm) {
    const left = OPENING_MS - performance.now();
    if (left > 0) await new Promise<void>(resolve => setTimeout(resolve, left));
  }
  document.body.classList.add("ready");
  document.body.setAttribute("aria-busy", "false");
  if (!loader || !watched) {
    loader?.remove();
    return;
  }
  // The app is drawn underneath while the loading screen dissolves over it.
  document.body.classList.add("entering");
  loader.dataset.leaving = "";
  setTimeout(() => {
    loader.remove();
    document.body.classList.remove("entering");
  }, calm ? 250 : LEAVING_MS + 150);
}

export function failStartup(error: unknown): void {
  console.error("League Companion startup failed", error);
  document.body.setAttribute("aria-busy", "false");
  const loader = document.querySelector<HTMLElement>("#startup-loader");
  if (loader) {
    loader.dataset.failed = "true";
    loader.removeAttribute("aria-label");
  }
  const message = document.querySelector<HTMLElement>("#startup-error");
  if (message) message.hidden = false;
}
