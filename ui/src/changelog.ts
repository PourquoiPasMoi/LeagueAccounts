// "What's new": the changes of each version in plain words. It opens once
// after the app was updated, and from Settings › Updates. When a newer version
// is waiting, its release notes are shown on top with the button to install it.
import type { Release } from "./api";
import { lang, localeTag, t, type Lang } from "./i18n";
import { esc, modal } from "./dom";
import entries from "./data/changelog.json";

interface Entry {
  version: string;
  /** YYYY-MM-DD. */
  date: string;
  /** The changes, one sentence each, per language (English when one is missing). */
  items: Partial<Record<Lang, string[]>>;
}

const LOG = entries as Entry[];
const SEEN = "changelogSeen";

/** "5.10.0" > "5.9.1": compared number by number. */
export function newer(a: string, b: string): boolean {
  const parts = (version: string) => version.replace(/^v/, "").split(/[.-]/).map((part) => Number.parseInt(part, 10) || 0);
  const [left, right] = [parts(a), parts(b)];
  for (let index = 0; index < Math.max(left.length, right.length); index++) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference) return difference > 0;
  }
  return false;
}

function seen(): string {
  try {
    return localStorage.getItem(SEEN) ?? "";
  } catch {
    return "";
  }
}

function remember(version: string): void {
  try {
    localStorage.setItem(SEEN, version);
  } catch {
    // Not kept: the window may open once more.
  }
}

/** Release notes written in simple Markdown (headings, lists, bold), as safe HTML. */
function notesHtml(notes: string): string {
  const inline = (text: string) => esc(text).replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>").replace(/`([^`]+)`/g, "$1");
  const lines = notes.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  return lines
    .map((line) => {
      const heading = /^#{1,6}\s+(.*)$/.exec(line);
      if (heading) return `<h4>${inline(heading[1])}</h4>`;
      const item = /^[-*]\s+(.*)$/.exec(line);
      return `<p class="${item ? "cl-item" : "cl-text"}">${inline(item ? item[1] : line)}</p>`;
    })
    .join("");
}

const dateText = (date: string): string => {
  const parsed = new Date(`${date}T12:00:00`);
  return Number.isNaN(parsed.getTime()) ? date : parsed.toLocaleDateString(localeTag(), { day: "numeric", month: "long", year: "numeric" });
};

export interface ChangelogOptions {
  /** The version running now. */
  current: string;
  /** Versions after this one are marked as new ("" = none). */
  since?: string;
  /** Opened by the app after an update (says so on top). */
  updated?: boolean;
  /** A newer release that is not installed yet, and how to install it. */
  upcoming?: Release | null;
  install?: () => void;
}

export async function openChangelog(options: ChangelogOptions): Promise<void> {
  const language = lang();
  const upcoming = options.upcoming && newer(options.upcoming.version, options.current) ? options.upcoming : null;
  const block = (entry: Entry, index: number) => {
    const items = entry.items[language] ?? entry.items.en ?? [];
    const fresh = options.since !== undefined && options.since !== "" && newer(entry.version, options.since) && !newer(entry.version, options.current);
    const installed = entry.version === options.current;
    return `
      <section class="cl-entry${fresh ? " is-new" : ""}" style="--k:${Math.min(index, 8)}">
        <header>
          <b>${esc(entry.version)}</b>
          ${fresh ? `<em class="cl-tag is-new">${esc(t("changelog.new"))}</em>` : installed ? `<em class="cl-tag">${esc(t("changelog.installed"))}</em>` : ""}
          <span>${esc(dateText(entry.date))}</span>
        </header>
        <div class="cl-items">${items.map((item) => `<p class="cl-item">${esc(item)}</p>`).join("")}</div>
      </section>`;
  };
  // Only versions up to the one running: the file may already describe the next one.
  const shown = LOG.filter((entry) => !newer(entry.version, options.current));
  const next = upcoming
    ? `
      <section class="cl-entry cl-upcoming" style="--k:0">
        <header><b>${esc(t("changelog.available", { version: upcoming.version }))}</b></header>
        <p class="cl-note">${esc(t("changelog.availableNote"))}</p>
        <div class="cl-items">${notesHtml(upcoming.notes)}</div>
        ${options.install ? `<button type="button" class="btn btn-primary" data-cl-install>${esc(t("changelog.update"))}</button>` : ""}
      </section>`
    : "";
  remember(options.current);
  await modal({
    title: t("changelog.title"),
    wide: true,
    className: "changelog-modal",
    body: `
      <p class="hint">${esc(options.updated ? t("changelog.updated", { version: options.current }) : t("changelog.intro"))}</p>
      <div class="cl-list">${next}${shown.map(block).join("")}</div>`,
    actions: [{ label: t("common.close"), value: "close", kind: "primary" }],
    onOpen: (root) => {
      root.querySelector("[data-cl-install]")?.addEventListener("click", () => {
        root.querySelector<HTMLElement>(".modal-close")?.click();
        options.install?.();
      });
    },
  });
}

/**
 * After an update: show what changed since the version last seen. A new
 * install (`firstRun`) only remembers the version, without opening anything.
 */
export function showChangelogAfterUpdate(current: string, firstRun: boolean): boolean {
  const last = seen();
  if (last === current) return false;
  if (firstRun) {
    remember(current);
    return false;
  }
  // Before this window existed nothing was remembered: everything from the
  // last version without it counts as new.
  const since = last || "5.8.1";
  if (!newer(current, since)) {
    remember(current);
    return false;
  }
  void openChangelog({ current, since, updated: true });
  return true;
}
