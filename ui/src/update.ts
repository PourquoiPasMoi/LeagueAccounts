// Automatic updates: the launch update screen, the "Restart now / Later" choice
// once an update is installed, and a banner that installs it directly when
// automatic updates are off or an install failed. Nothing opens GitHub.
import { ApiError, api, events, type Release, type UpdateStatus } from "./api";
import { $, esc, fragment, leave, toast } from "./dom";
import { errorText, lang, t } from "./i18n";
import { icon } from "./icons";

const DISMISSED_UPDATE = "leagueaccounts.dismissedUpdate";
/** Hold the loading screen up to this long from the start of the launch check. */
const LAUNCH_HOLD_MS = 3000;

let current: UpdateStatus | null = null;
const changed = new Set<() => void>();

let screen: HTMLElement | null = null;
let revealAfterScreen: (() => Promise<void>) | null = null;
/** The app was shown before the download started: track it in a banner. */
let progressBanner: HTMLElement | null = null;
/** "Continue in the background" or dismissing the progress banner. */
let silent = false;
let holding = false;
let notice: HTMLElement | null = null;
/** "Later" on the restart choice: the new version starts next launch. */
let declined = false;

const describe = (error: unknown): string =>
  error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error));

/** Subscribe before reading the state, so no change in between is missed. */
// `busy` is kept for callers; the restart is always the user's choice now.
export async function wireUpdates(_options: { busy: () => boolean }): Promise<void> {
  await events.onUpdateStatus(apply);
  apply(await api.updateStatus());
}

function apply(status: UpdateStatus): void {
  if (current && status.seq < current.seq) return;
  const previous = current;
  current = status;
  changed.forEach((listener) => listener());
  renderScreen();
  renderProgress();
  renderResult();
  const { phase, release } = status;
  if (phase === "downloading" && !screen && !silent) showProgress();
  if (phase !== "downloading") removeProgress();
  if ((phase === "available" || phase === "failed") && previous?.phase !== phase && release) {
    if (screen) void closeScreen();
    showUpdateBanner(release, phase === "failed");
  }
  if (phase === "ready" && previous?.phase !== "ready") {
    if (screen) showScreenChoice();
    else if (!holding) askToRestart();
  }
}

// ---------------------------------------------------------------- launch screen

const launchPending = (): boolean =>
  !!current?.launch && (current.phase === "checking" || (current.phase === "idle" && current.seq === 0));

/**
 * Hold the loading screen briefly for the launch check. When it found an update,
 * show the update screen instead of the app and return true; `reveal` then runs
 * if the user continues in the background or the install fails.
 */
export async function launchUpdate(reveal: () => Promise<void>): Promise<boolean> {
  holding = true;
  // Start the budget here, not at navigation: a cold WebView and a large
  // account list can take longer than the hold by the time this runs.
  if (launchPending()) {
    await new Promise<void>((resolve) => {
      const finish = () => {
        changed.delete(check);
        clearTimeout(timer);
        resolve();
      };
      const check = () => {
        if (!launchPending()) finish();
      };
      const timer = setTimeout(finish, LAUNCH_HOLD_MS);
      changed.add(check);
    });
  }
  holding = false;
  const phase = current?.phase;
  if (!current?.launch || (phase !== "downloading" && phase !== "ready")) {
    if (phase === "ready") askToRestart();
    return false;
  }
  const loader = document.querySelector<HTMLElement>("#startup-loader");
  if (!loader) return false;
  revealAfterScreen = reveal;
  // The app was never shown, so the loading screen is the only progress UI.
  removeProgress();
  loader.setAttribute("aria-label", t("update.screen"));
  screen = fragment(`
    <div class="update-screen">
      <p class="update-screen-title">${esc(t("update.screen"))}</p>
      <div class="update-meter" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-label="${esc(t("update.screen"))}"><span></span></div>
      <p class="update-screen-detail"></p>
      <div class="update-screen-actions">
        <button type="button" class="text-button" data-screen="background">${esc(t("update.background"))}</button>
      </div>
    </div>`);
  loader.append(screen);
  screen.addEventListener("click", (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-screen]")?.dataset.screen;
    if (action === "background") {
      silent = true;
      void closeScreen();
    }
    if (action === "restart") {
      screen?.querySelectorAll("button").forEach((button) => (button.disabled = true));
      void restart();
    }
    if (action === "later") {
      declined = true;
      void closeScreen();
      toast("info", t("update.nextLaunch", { version: current?.release?.version ?? "" }), "", 6000);
    }
  });
  renderScreen();
  if (phase === "ready") showScreenChoice();
  return true;
}

/** The download finished on the update screen: restart now, or open the app and update next time. */
function showScreenChoice(): void {
  if (!screen) return;
  $(".update-screen-title", screen).textContent = t("update.ready");
  $(".update-screen-actions", screen).innerHTML = `
    <button type="button" class="btn btn-primary" data-screen="restart">${icon("refresh-cw", 15)}${esc(t("update.restartNow"))}</button>
    <button type="button" class="btn btn-ghost" data-screen="later">${esc(t("update.later"))}</button>`;
  renderScreen();
}

const megabytes = (bytes: number): string =>
  new Intl.NumberFormat(lang(), { minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(bytes / 1048576);

function renderScreen(): void {
  if (!screen || !current) return;
  const installed = current.phase === "ready" || current.phase === "restarting";
  const fraction = installed ? 1 : current.total ? Math.min(1, current.received / current.total) : 0;
  // CSSOM, not a style attribute: the packaged CSP blocks inline style attributes.
  $("span", $(".update-meter", screen)).style.transform = `scaleX(${fraction})`;
  $(".update-meter", screen).setAttribute("aria-valuenow", String(Math.floor(fraction * 100)));
  $(".update-screen-detail", screen).textContent = installed
    ? t(current.phase === "restarting" ? "update.applying" : "update.installed", { version: current.release?.version ?? "" })
    : current.total
      ? `${megabytes(current.received)} / ${megabytes(current.total)} MB`
      : "";
}

/**
 * The launch check outlived the loading screen: show the download where the
 * app already is, so a slow check still has visible progress.
 */
function showProgress(): void {
  // Only after the app is visible: before that, launchUpdate owns the screen.
  if (progressBanner || !current?.release || !document.body.classList.contains("ready")) return;
  document.querySelector(".update-banner")?.remove();
  const label = t("update.downloading", { version: current.release.version });
  const element = fragment(`
    <div class="update-banner" role="status" data-update-progress>
      <div class="update-body">
        <strong>${esc(label)}</strong>
        <div class="update-meter" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-label="${esc(label)}"><span></span></div>
        <span class="update-progress-detail"></span>
      </div>
      <button class="icon-btn" data-update-progress="close" aria-label="${esc(t("common.close"))}" data-tooltip="${esc(t("common.close"))}">${icon("x")}</button>
    </div>`);
  progressBanner = element;
  $("#toasts").before(element);
  // Only the close button dismisses: the body is status text, not a control.
  $("button[data-update-progress='close']", element).addEventListener("click", () => {
    silent = true;
    removeProgress();
  });
  renderProgress();
}

function renderProgress(): void {
  if (!progressBanner || !current) return;
  const fraction = current.total ? Math.min(1, current.received / current.total) : 0;
  $("span", $(".update-meter", progressBanner)).style.transform = `scaleX(${fraction})`;
  $(".update-meter", progressBanner).setAttribute("aria-valuenow", String(Math.floor(fraction * 100)));
  $(".update-progress-detail", progressBanner).textContent = current.total
    ? `${megabytes(current.received)} / ${megabytes(current.total)} MB`
    : "";
}

function removeProgress(): void {
  progressBanner?.remove();
  progressBanner = null;
}

/** Show the app; an installed update then waits for an idle moment instead. */
async function closeScreen(): Promise<void> {
  const reveal = revealAfterScreen;
  screen?.remove();
  screen = null;
  revealAfterScreen = null;
  await reveal?.();
  if (current?.phase === "ready") askToRestart();
}

async function restart(): Promise<boolean> {
  try {
    await api.restartToUpdate();
    return true;
  } catch (error) {
    // A login or refresh is running: keep the choice up so it can be retried.
    toast("info", t("update.busy"), describe(error), 5000);
    if (screen) await closeScreen();
    else askToRestart();
    return false;
  }
}

// ---------------------------------------------------------------- restart choice

/** Ask once the update is installed: restart now, or keep going until the app is closed. */
function askToRestart(): void {
  if (declined || notice) return;
  const version = current?.release?.version ?? "";
  document.querySelector(".update-banner:not([data-update-progress])")?.remove();
  const element = fragment(`
    <div class="update-banner" role="status" data-update-notice>
      <div class="update-body">
        <strong>${esc(t("update.ready"))}</strong>
        <span>${esc(t("update.installed", { version }))}</span>
      </div>
      <button class="btn btn-primary" data-update="restart">${icon("refresh-cw", 15)}${esc(t("update.restartNow"))}</button>
      <button class="btn btn-ghost" data-update="later">${esc(t("update.later"))}</button>
    </div>`);
  notice = element;
  const close = () => {
    if (notice === element) notice = null;
    void leave(element, "toast-out");
  };
  $("#toasts").before(element);
  element.addEventListener("click", async (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-update]")?.dataset.update;
    if (action === "restart") {
      element.querySelectorAll("button").forEach((button) => (button.disabled = true));
      close();
      await restart();
    }
    if (action === "later") {
      declined = true;
      close();
      toast("info", t("update.nextLaunch", { version }), "", 6000);
    }
  });
}

// ---------------------------------------------------------------- download banner

/** A newer release that wasn't installed yet: Update now downloads and installs it here. */
export function showUpdateBanner(release: Release, failed = false): void {
  if (!failed) {
    try {
      if (localStorage.getItem(DISMISSED_UPDATE) === release.version) return;
    } catch {
      // Storage unavailable: show it.
    }
  }
  document.querySelector(".update-banner:not([data-update-progress])")?.remove();
  const banner = fragment(`
    <div class="update-banner" role="status">
      <div class="update-body">
        <strong>${esc(t(failed ? "update.failedTitle" : "update.available"))}</strong>
        <span>${esc(t(failed ? "update.failedText" : "update.text", { version: release.version }))}</span>
      </div>
      <button class="btn btn-primary" data-update="install">${icon("download", 15)}${esc(t(failed ? "update.retry" : "update.installNow"))}</button>
      <button class="icon-btn" data-update="later" aria-label="${esc(t("update.later"))}" data-tooltip="${esc(t("update.later"))}">${icon("x")}</button>
    </div>`);
  $("#toasts").before(banner);
  banner.addEventListener("click", (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-update]")?.dataset.update;
    if (action === "install") {
      silent = false;
      void leave(banner, "toast-out");
      api.checkUpdate().then(apply, (error) => toast("error", t("toast.updateFailed"), describe(error), 5000));
    }
    if (action === "later") {
      try {
        localStorage.setItem(DISMISSED_UPDATE, release.version);
      } catch {
        // Non-essential.
      }
      void leave(banner, "toast-out");
    }
  });
}

// ---------------------------------------------------------------- Settings › Check now

export async function checkUpdateNow(button: HTMLButtonElement, output: HTMLElement): Promise<void> {
  button.disabled = true;
  output.dataset.live = "";
  output.textContent = "…";
  try {
    silent = false;
    apply(await api.checkUpdate());
    renderResult();
  } catch (error) {
    delete output.dataset.live;
    output.textContent = "";
    toast("error", t("toast.updateFailed"), describe(error), 5000);
  } finally {
    button.disabled = false;
  }
}

/** Keep the Settings line current while a check or install it started runs. */
function renderResult(): void {
  const output = document.querySelector<HTMLElement>(".update-result[data-live]");
  if (!output || !current) return;
  const version = current.release?.version ?? "";
  const text: Record<UpdateStatus["phase"], string> = {
    idle: "",
    checking: "…",
    upToDate: t(current.release ? "update.latest" : "update.none"),
    available: t("update.text", { version }),
    failed: t("update.failedText", { version }),
    downloading: t("update.downloading", { version }),
    ready: t("update.installed", { version }),
    restarting: t("update.applying"),
  };
  output.textContent = text[current.phase];
}
