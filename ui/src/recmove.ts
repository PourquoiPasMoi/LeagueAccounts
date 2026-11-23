// Moving the recordings, clips and their data to another recordings folder:
// the question after the folder was changed (or "Bring in from another
// folder"), and the progress of the move, which runs in the background.

import { ApiError, api, events, type MoveStatus, type MoveSurvey } from "./api";
import { esc, modal, toast } from "./dom";
import { errorText, localeTag, t } from "./i18n";

const describe = (error: unknown): string => (error instanceof ApiError ? errorText(error.code, error.detail) : String(error));

function bytes(value: number): string {
  const format = (amount: number, unit: string) => `${new Intl.NumberFormat(localeTag(), { maximumFractionDigits: amount < 10 ? 1 : 0 }).format(amount)} ${unit}`;
  if (value >= 1e9) return format(value / 1e9, "GB");
  if (value >= 1e6) return format(value / 1e6, "MB");
  return format(value / 1e3, "KB");
}

/** One line for the end of a move: what arrived and what stayed behind. */
function result(status: MoveStatus): string {
  const lines = [status.stopped ? t("move.stopped") : t("move.done", { count: String(status.doneFiles), size: bytes(status.doneBytes) })];
  if (status.kept) lines.push(t("move.kept", { count: String(status.kept) }));
  if (status.failed) lines.push(t("move.failed", { count: String(status.failed) }));
  return lines.join(" ");
}

/** Where the files come from and where they go. */
const pathLines = (from: string, to: string): string =>
  `<p class="rec-convert-sub rec-move-paths">${esc(t("move.from", { path: from }))}<br>${esc(t("move.to", { path: to }))}</p>`;

let progressOpen = false;

/** Told once at startup: a toast when a move ends while its window is closed. */
export function watchRecordingsMove(): void {
  void events.onRecordingsMove((status) => {
    if (!status.finished || progressOpen) return;
    // A move continued silently at startup that found nothing to do says nothing.
    if (!status.doneFiles && !status.failed && !status.kept) return;
    toast(status.failed ? "error" : "success", t("move.title"), result(status), 6000);
  });
}

function progressBody(status: MoveStatus): string {
  const paths = pathLines(status.from, status.to);
  if (!status.running) {
    if (!status.finished) return `<p class="rec-convert-line">${esc(t("move.idle"))}</p>`;
    return `<p class="rec-convert-line">${esc(result(status))}</p>${status.failed ? `<p class="rec-convert-sub">${esc(t("move.failedHint"))}</p>` : ""}${paths}`;
  }
  const share = status.totalBytes ? Math.min(1, status.doneBytes / status.totalBytes) : 0;
  const line = status.paused ? t("move.paused") : t("move.running", { done: String(status.doneFiles), total: String(status.totalFiles) });
  return `<p class="rec-convert-line">${esc(line)}</p>
    <div class="rec-convert-meter" style="--convert: ${share.toFixed(4)}"><span></span></div>
    <p class="rec-convert-sub">${esc(t("move.amount", { done: bytes(status.doneBytes), total: bytes(status.totalBytes) }))}${status.current ? ` · ${esc(status.current)}` : ""}</p>
    <p class="rec-convert-sub">${esc(t("move.background"))}</p>${paths}
    <div class="rec-convert-actions"><button type="button" class="btn btn-ghost" data-move="stop">${esc(t("move.stop"))}</button></div>`;
}

/** The move's progress (it keeps running when this is closed). */
export async function openMoveProgress(first?: MoveStatus): Promise<void> {
  if (progressOpen) return;
  progressOpen = true;
  let status = first ?? (await api.recordingsMoveStatus().catch(() => null));
  const listening: { stop?: () => void; closed?: boolean } = {};
  await modal({
    title: t("move.title"),
    body: `<div class="rec-convert-body" data-move-body>${status ? progressBody(status) : ""}</div>`,
    actions: [{ label: t("common.close"), value: "close" }],
    className: "rec-convert-modal",
    onOpen: (root) => {
      const area = root.querySelector<HTMLElement>("[data-move-body]")!;
      void events
        .onRecordingsMove((next) => {
          status = next;
          area.innerHTML = progressBody(next);
        })
        .then((stop) => {
          if (listening.closed) stop();
          else listening.stop = stop;
        });
      area.addEventListener("click", (event) => {
        if ((event.target as HTMLElement).closest('[data-move="stop"]')) void api.stopRecordingsMove();
      });
    },
  });
  listening.closed = true;
  listening.stop?.();
  progressOpen = false;
}

function surveyBody(survey: MoveSurvey, short: boolean): string {
  const how = survey.sameDrive ? t("move.askSameDrive") : t("move.askCopy");
  return `<div class="rec-convert-body">
    <p class="rec-convert-line">${esc(t("move.askCount", { videos: String(survey.videos), size: bytes(survey.bytes) }))}</p>
    ${pathLines(survey.from, survey.to)}
    <p>${esc(t("move.askWhat"))} ${esc(how)}</p>
    ${short ? `<p class="rec-move-warning">${esc(t("move.askNoSpace", { need: bytes(survey.bytes), free: bytes(survey.freeBytes ?? 0) }))}</p>` : ""}
  </div>`;
}

/**
 * Ask whether the recordings in `from` should be moved into the folder in
 * use, and start the move. `quiet`: say nothing when `from` holds no recordings
 * (the folder was just changed and the old one was empty).
 */
export async function offerRecordingsMove(from: string, quiet = false): Promise<void> {
  let survey: MoveSurvey;
  try {
    const running = await api.recordingsMoveStatus();
    if (running.running) {
      void openMoveProgress(running);
      return;
    }
    survey = await api.surveyRecordingsMove(from);
  } catch (error) {
    if (!quiet) toast("error", t("move.title"), describe(error));
    return;
  }
  if (!survey.files) {
    if (!quiet) toast("info", t("move.title"), t("move.nothing"));
    return;
  }
  const short = !survey.sameDrive && survey.freeBytes != null && survey.freeBytes < survey.bytes;
  const answer = await modal({
    title: t("move.askTitle"),
    body: surveyBody(survey, short),
    actions: [{ label: t("move.leave"), kind: "ghost", value: "leave" }, ...(short ? [] : [{ label: t("move.start"), kind: "primary" as const, value: "move" }])],
  });
  if (answer !== "move") return;
  try {
    void openMoveProgress(await api.startRecordingsMove(from));
  } catch (error) {
    toast("error", t("move.title"), describe(error));
  }
}
