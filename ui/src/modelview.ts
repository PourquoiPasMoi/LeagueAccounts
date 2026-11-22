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
    api.showModelViewer(skinId, ...box()).catch(() => {
      toast("error", t("toast.browserFailed"));
      close();
    });
  }, 260);
}
