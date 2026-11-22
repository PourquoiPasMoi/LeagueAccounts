import { api } from "./api";
import { esc, fragment, toast } from "./dom";
import { icon } from "./icons";
import { t } from "./i18n";

let closeCurrent: (() => void) | null = null;

/**
 * A champion or skin in 3D, inside the app: a panel of ours (title, close button) whose stage
 * is kept free for a second webview that the app lays exactly over it. That webview shows
 * Khada's model viewer (modelviewer.lol) with only the model left visible. It is a native
 * layer above the page, so nothing of ours may be drawn over the stage.
 */
export function openModelView(skinId: number, name: string): void {
  closeCurrent?.();
  const overlay = fragment(`
    <div class="overlay model-overlay" role="dialog" aria-modal="true" aria-label="${esc(name)}">
      <div class="model-panel">
        <header class="model-head">
          <h3>${esc(name)}</h3>
          <span class="muted">${esc(t("profile.view3dCredit"))}</span>
          <div class="model-chromas" role="group" aria-label="${esc(t("profile.chromasLabel"))}" hidden></div>
          <button class="icon-btn" data-model-close aria-label="${esc(t("common.close"))}">${icon("x", 16)}</button>
        </header>
        <div class="model-stage"><p>${esc(t("profile.view3dLoading"))}</p></div>
      </div>
    </div>`);
  (document.querySelector("#modal-root") ?? document.body).append(overlay);
  const stage = overlay.querySelector<HTMLElement>(".model-stage")!;
  // The window's own pixels: CSS pixels times the page zoom and the screen scale.
  const box = (): [number, number, number, number] => {
    const rect = stage.getBoundingClientRect();
    const ratio = window.devicePixelRatio || 1;
    return [rect.left * ratio, rect.top * ratio, rect.width * ratio, rect.height * ratio];
  };
  let open = true;
  let chroma: number | null = null;
  const show = () =>
    api.showModelViewer(skinId, chroma, ...box()).catch(() => {
      toast("error", t("toast.browserFailed"));
      close();
    });
  // The skin's chromas as colour swatches: a click shows the model in that chroma.
  const swatches = overlay.querySelector<HTMLElement>(".model-chromas")!;
  const colour = (value: string) => (/^#[0-9a-f]{6}$/i.test(value) ? value : "#888888");
  void api.championSkins(Math.floor(skinId / 1000)).then((skins) => {
    const list = skins.find((skin) => skin.id === skinId)?.chromaList ?? [];
    if (!open || !list.length) return;
    swatches.innerHTML =
      `<button class="model-chroma is-none" data-chroma="" aria-pressed="true" data-tooltip="${esc(t("profile.chromaNone"))}"></button>` +
      list.map((entry) => `<button class="model-chroma" data-chroma="${entry.id}" aria-pressed="false" data-tooltip="${esc(entry.name)}" style="background:linear-gradient(135deg, ${colour(entry.colors[0] ?? "")} 50%, ${colour(entry.colors[1] ?? entry.colors[0] ?? "")} 50%)"></button>`).join("");
    swatches.hidden = false;
    // The header grew: the stage moved.
    requestAnimationFrame(place);
  }).catch(() => undefined);
  swatches.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLElement>("[data-chroma]");
    if (!button) return;
    chroma = button.dataset.chroma ? Number(button.dataset.chroma) : null;
    swatches.querySelectorAll("[data-chroma]").forEach((other) => other.setAttribute("aria-pressed", String(other === button)));
    void show();
  });
  const place = () => {
    if (open) void api.placeModelViewer(...box()).catch(() => undefined);
  };
  const close = () => {
    if (!open) return;
    open = false;
    closeCurrent = null;
    window.removeEventListener("resize", place);
    document.removeEventListener("keydown", onKey, true);
    void api.closeModelViewer().catch(() => undefined);
    overlay.remove();
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopImmediatePropagation();
    close();
  };
  closeCurrent = close;
  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay || (event.target as HTMLElement).closest("[data-model-close]")) close();
  });
  document.addEventListener("keydown", onKey, true);
  window.addEventListener("resize", place);
  // After the panel's entrance has settled, so the stage is where it will stay.
  setTimeout(() => {
    if (!open) return;
    void show();
  }, 260);
}
