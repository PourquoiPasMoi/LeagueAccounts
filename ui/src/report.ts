// Report builder: choose what goes into a patch notes image, watch it redraw
// live, and save it in high resolution under a chosen file size (for Discord
// and the like). The picture itself is drawn by `reportimage.ts`.
import { ApiError, api, type PatchBlock } from "./api";
import { $, esc, fragment, leave, toast } from "./dom";
import { icon } from "./icons";
import { t } from "./i18n";
import { exportReport, paintReport, type Exported, type ReportOptions, type ReportSection } from "./reportimage";

export interface ReportSource {
  title: string;
  subtitle: string;
  sections: ReportSection[];
  /** Start with every card ticked (search results); else champions and items only. */
  whole?: boolean;
}

/// Megabytes: Discord's free upload limit is 10 MB.
const MAX_MB = 9.7;
const PREFS = "pnReport";

interface Prefs {
  columns: number;
  quotes: boolean;
  pictures: boolean;
  credit: boolean;
  sharp: number;
  megabytes: number;
}

function prefs(): Prefs {
  const base: Prefs = { columns: 3, quotes: false, pictures: false, credit: true, sharp: 2, megabytes: MAX_MB };
  try {
    const saved = JSON.parse(localStorage.getItem(PREFS) ?? "{}") as Partial<Prefs>;
    return {
      columns: Math.min(5, Math.max(1, Number(saved.columns) || base.columns)),
      quotes: typeof saved.quotes === "boolean" ? saved.quotes : base.quotes,
      pictures: typeof saved.pictures === "boolean" ? saved.pictures : base.pictures,
      credit: typeof saved.credit === "boolean" ? saved.credit : base.credit,
      sharp: [1, 2, 3].includes(Number(saved.sharp)) ? Number(saved.sharp) : base.sharp,
      megabytes: Math.min(MAX_MB, Math.max(1, Number(saved.megabytes) || base.megabytes)),
    };
  } catch {
    return base;
  }
}

const megabytes = (bytes: number): string => (bytes / (1024 * 1024)).toFixed(bytes < 1024 * 1024 * 10 ? 2 : 1);

let opened = false;

export function openReport(source: ReportSource): void {
  if (opened) return;
  opened = true;
  const state = prefs();
  let title = source.title;
  let subtitle = source.subtitle;
  // Champions and items are what people share most: they start ticked. A source without them starts whole.
  const typed = !source.whole && source.sections.some((section) => section.blocks.some((block) => block.kind));
  const picked = source.sections.map((section) => new Set(section.blocks.map((_, index) => index).filter((index) => !typed || Boolean(section.blocks[index].kind))));
  const unfolded = new Set<number>();
  let fit = true;
  /// Each change gets a number: slower, older work is dropped.
  let turn = 0;
  let ready: Exported | null = null;
  let saved = "";
  let saving = false;

  const overlay = fragment(`
    <div class="overlay pr-overlay">
      <div class="modal pr-modal" role="dialog" aria-modal="true" aria-label="${esc(t("report.title"))}">
        <div class="modal-close-mount">
          <button class="modal-close" data-pr="close" aria-label="${esc(t("common.close"))}"><span></span></button>
        </div>
        <header class="pr-head"><h2>${esc(t("report.title"))}</h2><p>${esc(t("report.intro"))}</p></header>
        <div class="pr-layout">
          <aside class="pr-options">
            <div class="pr-scroll">
              <section class="pr-group">
                <label class="field"><span>${esc(t("report.heading"))}</span><input type="text" data-pr-title maxlength="80" value="${esc(title)}" spellcheck="false"/></label>
                <label class="field"><span>${esc(t("report.subheading"))}</span><input type="text" data-pr-subtitle maxlength="160" value="${esc(subtitle)}" spellcheck="false"/></label>
              </section>
              <section class="pr-group">
                <header class="pr-group-head"><h3>${esc(t("report.content"))}</h3><button type="button" class="pr-link" data-pr="all">${esc(t("report.all"))}</button><button type="button" class="pr-link" data-pr="none">${esc(t("report.none"))}</button></header>
                <div class="pr-sections"></div>
              </section>
              <section class="pr-group">
                <header class="pr-group-head"><h3>${esc(t("report.layout"))}</h3></header>
                <div class="pr-line"><span>${esc(t("report.columns"))}</span><div class="segmented" data-pr-columns>${[1, 2, 3, 4, 5].map((count) => `<button type="button" data-value="${count}">${count}</button>`).join("")}</div></div>
                <label class="toggle-row"><span class="toggle-text"><strong>${esc(t("report.quotes"))}</strong><span>${esc(t("report.quotesHint"))}</span></span><input type="checkbox" data-pr-toggle="quotes"/><span class="switch"></span></label>
                <label class="toggle-row"><span class="toggle-text"><strong>${esc(t("report.pictures"))}</strong><span>${esc(t("report.picturesHint"))}</span></span><input type="checkbox" data-pr-toggle="pictures"/><span class="switch"></span></label>
                <label class="toggle-row"><span class="toggle-text"><strong>${esc(t("report.credit"))}</strong></span><input type="checkbox" data-pr-toggle="credit"/><span class="switch"></span></label>
              </section>
              <section class="pr-group">
                <header class="pr-group-head"><h3>${esc(t("report.file"))}</h3></header>
                <div class="pr-line"><span>${esc(t("report.sharpness"))}</span><div class="segmented" data-pr-sharp><button type="button" data-value="1">${esc(t("report.sharp1"))}</button><button type="button" data-value="2">${esc(t("report.sharp2"))}</button><button type="button" data-value="3">${esc(t("report.sharp3"))}</button></div></div>
                <div class="pr-line pr-size"><span>${esc(t("report.maxSize"))}</span><b data-pr-size-value></b></div>
                <div class="gs-slider"><input type="range" data-pr-size min="1" max="${MAX_MB}" step="0.1" aria-label="${esc(t("report.maxSize"))}"/></div>
              </section>
            </div>
            <footer class="pr-foot">
              <p class="pr-info" data-pr-info></p>
              <div class="pr-actions">
                <button type="button" class="btn btn-ghost" data-pr="show" hidden>${icon("folder-open", 14)}<span>${esc(t("report.show"))}</span></button>
                <button type="button" class="btn btn-primary" data-pr="save">${icon("download", 14)}<span>${esc(t("report.save"))}</span></button>
              </div>
            </footer>
          </aside>
          <section class="pr-preview" data-fit="true">
            <div class="pr-stage"><canvas class="pr-canvas"></canvas></div>
            <p class="pr-empty" hidden>${esc(t("report.empty"))}</p>
            <button type="button" class="pr-zoom btn btn-ghost btn-small" data-pr="zoom"></button>
          </section>
        </div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  $("#modal-root").append(overlay);
  const sections = $(".pr-sections", overlay);
  const preview = $(".pr-preview", overlay);
  const canvas = $<HTMLCanvasElement>(".pr-canvas", overlay);
  const empty = $(".pr-empty", overlay);
  const info = $("[data-pr-info]", overlay);
  const saveButton = $<HTMLButtonElement>('[data-pr="save"]', overlay);
  const showButton = $<HTMLButtonElement>('[data-pr="show"]', overlay);
  const zoomButton = $<HTMLButtonElement>('[data-pr="zoom"]', overlay);
  const sizeInput = $<HTMLInputElement>("[data-pr-size]", overlay);
  const sizeValue = $("[data-pr-size-value]", overlay);

  const remember = () => {
    try {
      localStorage.setItem(PREFS, JSON.stringify(state));
    } catch {
      // Not kept: the next report starts from the usual choices.
    }
  };

  const options = (): ReportOptions => ({
    title: title.trim(),
    subtitle: subtitle.trim(),
    sections: source.sections.map((section, at) => ({ title: section.title, blocks: section.blocks.filter((_, index) => picked[at].has(index)) })).filter((section) => section.blocks.length),
    columns: state.columns,
    quotes: state.quotes,
    pictures: state.pictures,
    credit: state.credit ? t("report.creditLine") : "",
  });

  const cardName = (block: PatchBlock, index: number): string => block.title || t("report.untitled", { number: index + 1 });

  const renderSections = () => {
    sections.innerHTML = source.sections
      .map((section, at) => {
        const count = picked[at].size;
        const total = section.blocks.length;
        const open = unfolded.has(at);
        const cards = open
          ? `<div class="pr-cards">${section.blocks.map((block, index) => `<label class="gi-check pr-check"><input type="checkbox" data-pr-card="${at}:${index}" ${picked[at].has(index) ? "checked" : ""}/><span>${esc(cardName(block, index))}</span></label>`).join("")}</div>`
          : "";
        return `
          <div class="pr-section${open ? " is-open" : ""}">
            <div class="pr-section-row">
              <label class="gi-check pr-check"><input type="checkbox" data-pr-section="${at}" ${count === total ? "checked" : ""} ${count > 0 && count < total ? "data-some" : ""}/><span>${esc(section.title || t("report.introSection"))}</span></label>
              <button type="button" class="pr-fold" data-pr-fold="${at}" aria-expanded="${open}" ${total > 1 ? "" : "hidden"}><em>${count}/${total}</em>${icon("chevron-down", 13)}</button>
            </div>
            ${cards}
          </div>`;
      })
      .join("");
    sections.querySelectorAll<HTMLInputElement>("[data-some]").forEach((box) => (box.indeterminate = true));
  };

  const setControls = () => {
    overlay.querySelectorAll<HTMLElement>("[data-pr-columns] button").forEach((button) => button.setAttribute("aria-pressed", String(Number(button.dataset.value) === state.columns)));
    overlay.querySelectorAll<HTMLElement>("[data-pr-sharp] button").forEach((button) => button.setAttribute("aria-pressed", String(Number(button.dataset.value) === state.sharp)));
    overlay.querySelectorAll<HTMLInputElement>("[data-pr-toggle]").forEach((box) => (box.checked = state[box.dataset.prToggle as "quotes" | "pictures" | "credit"]));
    sizeInput.value = String(state.megabytes);
    sizeValue.textContent = t("report.megabytes", { size: state.megabytes.toFixed(1) });
    zoomButton.textContent = t(fit ? "report.zoomIn" : "report.zoomFit");
    preview.dataset.fit = String(fit);
  };

  const setInfo = (text: string, busy = false) => {
    info.textContent = text;
    info.classList.toggle("is-busy", busy);
  };

  // -------------------------------------------------------------- drawing

  let drawTimer = 0;
  let fileTimer = 0;

  /** The file that Save will write, made ahead so its real size and format show. */
  const prepare = async (mine: number): Promise<Exported | null> => {
    const chosen = options();
    if (!chosen.sections.length) return null;
    setInfo(t("report.preparing"), true);
    const made = await exportReport(chosen, state.sharp, Math.floor(state.megabytes * 1024 * 1024), () => mine === turn && opened);
    if (mine !== turn || !opened) return null;
    ready = made;
    setInfo(made ? `${t("report.info", { width: made.width, height: made.height, size: megabytes(made.blob.size), format: made.extension.toUpperCase() })}${made.tall ? `\n${t("report.tall")}` : ""}` : t("report.failed"));
    return made;
  };

  const redraw = async () => {
    const mine = ++turn;
    ready = null;
    saved = "";
    showButton.hidden = true;
    window.clearTimeout(fileTimer);
    const chosen = options();
    const nothing = !chosen.sections.length;
    empty.hidden = !nothing;
    canvas.hidden = nothing;
    saveButton.disabled = nothing || saving;
    if (nothing) return setInfo("");
    setInfo(t("report.drawing"), true);
    // The preview is drawn off screen and swapped in, so it never flashes empty.
    const painted = await paintReport(chosen, Math.min(1.5, Math.max(1, window.devicePixelRatio || 1)));
    if (mine !== turn || !opened) return;
    canvas.width = painted.canvas.width;
    canvas.height = painted.canvas.height;
    canvas.getContext("2d")!.drawImage(painted.canvas, 0, 0);
    canvas.style.setProperty("--natural", `${painted.width}px`);
    canvas.classList.remove("is-fresh");
    void canvas.offsetWidth;
    canvas.classList.add("is-fresh");
    fileTimer = window.setTimeout(() => void prepare(mine), 700);
  };

  const changed = (wait = 140) => {
    window.clearTimeout(drawTimer);
    drawTimer = window.setTimeout(() => void redraw(), wait);
  };

  const save = async () => {
    if (saving) return;
    saving = true;
    saveButton.disabled = true;
    window.clearTimeout(fileTimer);
    try {
      const made = ready ?? (await prepare(turn));
      if (!made) return;
      const bytes = new Uint8Array(await made.blob.arrayBuffer());
      saved = await api.saveReportImage(bytes, title.trim() || source.title);
      showButton.hidden = false;
      toast("success", t("report.saved"), saved, 5200);
    } catch (error) {
      toast("error", t("report.saveFailed"), error instanceof ApiError ? "" : String(error));
    } finally {
      saving = false;
      saveButton.disabled = !options().sections.length;
    }
  };

  // -------------------------------------------------------------- input

  const close = () => {
    if (!opened) return;
    opened = false;
    turn += 1;
    window.clearTimeout(drawTimer);
    window.clearTimeout(fileTimer);
    document.removeEventListener("keydown", onKey, true);
    void leave(overlay, "overlay-out");
    previousFocus?.focus?.();
  };

  const onKey = (event: KeyboardEvent) => {
    if ([...document.querySelectorAll("#modal-root > .overlay:not(.overlay-out)")].at(-1) !== overlay) return;
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    close();
  };
  document.addEventListener("keydown", onKey, true);

  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) close();
  });

  overlay.addEventListener("input", (event) => {
    const target = event.target as HTMLInputElement;
    if (target.matches("[data-pr-title]")) {
      title = target.value;
      return changed(260);
    }
    if (target.matches("[data-pr-subtitle]")) {
      subtitle = target.value;
      return changed(260);
    }
    if (target.matches("[data-pr-size]")) {
      state.megabytes = Number(target.value);
      sizeValue.textContent = t("report.megabytes", { size: state.megabytes.toFixed(1) });
      remember();
      changed(450);
    }
  });

  overlay.addEventListener("change", (event) => {
    const target = event.target as HTMLInputElement;
    const toggle = target.dataset.prToggle as "quotes" | "pictures" | "credit" | undefined;
    if (toggle) {
      state[toggle] = target.checked;
      remember();
      return changed();
    }
    if (target.dataset.prSection !== undefined) {
      const at = Number(target.dataset.prSection);
      picked[at] = new Set(target.checked ? source.sections[at].blocks.map((_, index) => index) : []);
      renderSections();
      return changed();
    }
    if (target.dataset.prCard) {
      const [at, index] = target.dataset.prCard.split(":").map(Number);
      if (target.checked) picked[at].add(index);
      else picked[at].delete(index);
      renderSections();
      changed();
    }
  });

  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const action = target.closest<HTMLElement>("[data-pr]")?.dataset.pr;
    if (action === "close") return close();
    if (action === "save") return void save();
    if (action === "show") return void api.showReportImage(saved).catch(() => undefined);
    if (action === "zoom" || target === canvas) {
      fit = !fit;
      return setControls();
    }
    if (action === "all" || action === "none") {
      source.sections.forEach((section, at) => (picked[at] = new Set(action === "all" ? section.blocks.map((_, index) => index) : [])));
      renderSections();
      return changed();
    }
    const fold = target.closest<HTMLElement>("[data-pr-fold]");
    if (fold) {
      const at = Number(fold.dataset.prFold);
      if (unfolded.has(at)) unfolded.delete(at);
      else unfolded.add(at);
      return renderSections();
    }
    const column = target.closest<HTMLElement>("[data-pr-columns] button");
    if (column) {
      state.columns = Number(column.dataset.value);
      remember();
      setControls();
      return changed();
    }
    const sharp = target.closest<HTMLElement>("[data-pr-sharp] button");
    if (sharp) {
      state.sharp = Number(sharp.dataset.value);
      remember();
      setControls();
      changed();
    }
  });

  renderSections();
  setControls();
  void redraw();
}
