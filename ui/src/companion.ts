// Things around the accounts: the Overview (every account at a glance), the
// Notifications and Backup pages of Settings, and the feature guide shown once.
import { ApiError, api, championIcon, events, type AccountOverview, type BackupView, type ChampionOverview, type Key, type NotifyView, type Overview, type RemoteView } from "./api";
import { errorText, localeTag, t, tierName, type MessageKey } from "./i18n";
import { $, esc, modal, toast } from "./dom";
import { icon } from "./icons";
import { emblem, normalizeTier } from "./ranks";
import championIds from "./data/champion-ids.json";

const describe = (error: unknown) => (error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error)));
const DAY = 86_400_000;

function stored(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function store(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not kept: fine.
  }
}

// ---------------------------------------------------------------- notifications

/** Give Windows notifications their texts in the interface language. */
export function sendNotifyTexts(): void {
  const keys = ["decayTitle", "decayBody", "decayNowBody", "banTitle", "banBody", "recordingTitle", "recoveredTitle", "recoveredBody", "backupFailedTitle", "backupFailedBody", "testTitle", "testBody"];
  const texts = Object.fromEntries(keys.map((key) => [key, t(`notify.${key}` as MessageKey)]));
  void api.setNotifyTexts(texts).catch(() => undefined);
}

/** Give the phone page its texts in the interface language. */
export function sendRemoteTexts(): void {
  const keys = ["waiting", "waitingText", "found", "accept", "decline", "accepted", "declined", "ban", "pick", "soon", "wait", "done", "planning", "champions", "runes", "search", "lockBan", "lockPick", "pages", "spells", "auto", "autoDone", "failed", "pin", "pinWrong", "pinLocked", "pair", "none", "noPages", "autoOn", "acceptedAuto", "acceptNow", "primary", "secondary", "shards", "keep", "inLobby", "inQueue", "flowText", "cards", "yours", "bench", "benchEmpty", "reroll", "rerollsLeft", "back"];
  const texts = Object.fromEntries(keys.map((key) => [key, t(`remote.p.${key}` as MessageKey)]));
  void api.setRemoteTexts(texts).catch(() => undefined);
}

/** The note in the sidebar while the phone remote is on: where to find the page. */
export function showRemoteNote(view: RemoteView): void {
  const note = document.querySelector<HTMLElement>("#remote-note");
  if (!note) return;
  note.hidden = !view.enabled;
  const set = (selector: string, text: string) => {
    const element = note.querySelector<HTMLElement>(selector);
    if (element) element.textContent = text;
  };
  set("strong", t(view.running ? "remote.noteLive" : "remote.noteOn"));
  set("[data-remote-note-name]", view.name.replace(/^http:\/\//, ""));
  set("[data-remote-note-ip]", view.address.replace(/^http:\/\//, ""));
  note.classList.toggle("is-live", view.running);
  note.dataset.tooltip = t("remote.noteHint");
}

/** Read the phone remote's state and show the sidebar note; keeps it current. */
export function watchRemoteNote(): void {
  const refresh = () => void api.remoteInfo().then(showRemoteNote).catch(() => undefined);
  refresh();
  void events.onRemoteStatus(refresh);
}

/** Settings › Phone remote. Saves at once. */
export function remotePage(current: string): string {
  return `
    <section class="settings-page" data-settings-page="remote" ${current === "remote" ? "" : "hidden"} data-settings-ignore>
      <h3 class="settings-page-title">${esc(t("settings.remote"))}</h3>
      <p class="settings-note">${esc(t("remote.hint"))}</p>
      ${switchRow("remoteEnabled", t("remote.enabled"), t("remote.enabledHint"))}
      <div data-remote-details hidden>
        <div class="companion-row">
          <span class="toggle-text"><strong>${esc(t("remote.address"))}</strong><span class="remote-address" data-remote-name></span><span class="remote-address" data-remote-ip></span></span>
          <span class="remote-buttons">
            <button type="button" class="btn btn-ghost" data-remote-test>${esc(t("remote.test"))}</button>
          </span>
        </div>
        <p class="settings-note" data-remote-result hidden></p>
        <div class="companion-row">
          <span class="toggle-text"><strong>${esc(t("remote.pin"))}</strong><span>${esc(t("remote.pinHint"))}</span></span>
          <span class="remote-pin" data-remote-pin></span>
        </div>
        <div class="companion-row">
          <span class="toggle-text"><strong>${esc(t("remote.phones"))}</strong><span data-remote-paired></span></span>
          <button type="button" class="btn btn-ghost" data-remote-forget>${esc(t("remote.forget"))}</button>
        </div>
        <p class="settings-note" data-remote-status></p>
        <p class="settings-note">${esc(t("remote.firewall"))}</p>
      </div>
    </section>`;
}

// ---------------------------------------------------------------- settings pages

export const COMPANION_TABS = ["notifications", "backup"] as const;

/** The two entries of the Settings side list. */
export function companionNav(current: string): string {
  return COMPANION_TABS.map(
    (tab) => `<button type="button" role="tab" data-settings-tab="${tab}" aria-selected="${current === tab}">${esc(t(`settings.${tab}` as MessageKey))}</button>`,
  ).join("");
}

const switchRow = (name: string, label: string, hint: string) => `
  <label class="toggle-row">
    <span class="toggle-text"><strong>${esc(label)}</strong>${hint ? `<span>${esc(hint)}</span>` : ""}</span>
    <input type="checkbox" name="${name}"/>
    <span class="switch"></span>
  </label>`;

/** Both pages. They save at once (not with the Save button): `data-settings-ignore`. */
export function companionPages(current: string): string {
  const everyOptions: [number, MessageKey][] = [
    [1, "backup.everyDay"],
    [7, "backup.everyWeek"],
    [30, "backup.everyMonth"],
  ];
  return `
    <section class="settings-page" data-settings-page="notifications" ${current === "notifications" ? "" : "hidden"} data-settings-ignore>
      <h3 class="settings-page-title">${esc(t("settings.notifications"))}</h3>
      <p class="settings-note">${esc(t("notify.hint"))}</p>
      ${switchRow("notifyDecay", t("notify.decay"), t("notify.decayHint"))}
      ${switchRow("notifyBan", t("notify.ban"), t("notify.banHint"))}
      ${switchRow("notifyRecording", t("notify.recording"), t("notify.recordingHint"))}
      <div class="companion-row">
        <span></span>
        <button type="button" class="btn btn-ghost" data-notify-test>${esc(t("notify.test"))}</button>
      </div>
    </section>
    <section class="settings-page" data-settings-page="backup" ${current === "backup" ? "" : "hidden"} data-settings-ignore>
      <h3 class="settings-page-title">${esc(t("settings.backup"))}</h3>
      <p class="settings-note">${esc(t("backup.hint"))}</p>
      ${switchRow("backupEnabled", t("backup.enabled"), t("backup.enabledHint"))}
      <div class="companion-row">
        <span class="toggle-text"><strong>${esc(t("backup.folder"))}</strong><span data-backup-folder></span></span>
        <button type="button" class="btn btn-ghost" data-backup-pick>${icon("folder-open", 14)}${esc(t("backup.choose"))}</button>
      </div>
      <div class="companion-row">
        <span class="toggle-text"><strong>${esc(t("backup.passphrase"))}</strong><span data-backup-passphrase></span></span>
        <button type="button" class="btn btn-ghost" data-backup-set></button>
      </div>
      <label class="field inline"><span>${esc(t("backup.every"))}</span>
        <div class="select-wrap"><select name="backupEvery">${everyOptions.map(([days, key]) => `<option value="${days}">${esc(t(key))}</option>`).join("")}</select></div></label>
      <label class="field inline"><span>${esc(t("backup.keep"))}</span>
        <div class="select-wrap"><select name="backupKeep">${[3, 5, 10, 30].map((count) => `<option value="${count}">${count}</option>`).join("")}</select></div></label>
      <div class="companion-row">
        <span class="toggle-text"><strong>${esc(t("backup.last"))}</strong><span data-backup-last></span></span>
        <button type="button" class="btn btn-ghost" data-backup-now>${esc(t("backup.now"))}</button>
      </div>
      <p class="settings-note">${esc(t("backup.restoreHint"))}</p>
    </section>`;
}

/** Auto accept, shown on the Login page of Settings. Saves at once. */
export function autoAcceptSection(): string {
  return `
    <div data-settings-ignore data-auto-accept>
      ${switchRow("autoAccept", t("accept.enabled"), t("accept.hint"))}
      <label class="field inline"><span>${esc(t("accept.delay"))}</span>
        <div class="select-wrap"><select name="autoAcceptDelay">${[0, 1, 2, 3, 4, 5, 6].map((seconds) => `<option value="${seconds}">${esc(seconds ? t("accept.seconds", { n: seconds }) : t("accept.noDelay"))}</option>`).join("")}</select></div></label>
    </div>`;
}

/** Ask for a new passphrase (typed twice). Null when cancelled. */
async function askNewPassphrase(): Promise<string | null> {
  let value = "";
  const answer = await modal({
    title: t("backup.passphraseTitle"),
    body: `
      <p class="hint">${esc(t("backup.passphraseWarning"))}</p>
      <label class="field"><span>${esc(t("backup.passphrase"))}</span><input name="pass" type="password" autocomplete="new-password" minlength="8"/></label>
      <label class="field"><span>${esc(t("backup.passphraseAgain"))}</span><input name="again" type="password" autocomplete="new-password"/></label>
      <p class="field-error" data-pass-error hidden></p>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("common.save"), value: "save", kind: "primary" },
    ],
    onSubmit: (action, root) => {
      if (action !== "save") return true;
      const pass = $<HTMLInputElement>('[name="pass"]', root).value;
      const again = $<HTMLInputElement>('[name="again"]', root).value;
      const problem = pass.length < 8 ? t("err.backup_passphrase_short") : pass !== again ? t("backup.passphraseMismatch") : "";
      const error = $<HTMLElement>("[data-pass-error]", root);
      error.textContent = problem;
      error.hidden = !problem;
      value = pass;
      return !problem;
    },
  });
  return answer === "save" ? value : null;
}

/** Ask for the passphrase of a backup file. Null when cancelled. */
export async function askBackupPassphrase(): Promise<string | null> {
  let value = "";
  const answer = await modal({
    title: t("backup.openTitle"),
    body: `
      <p class="hint">${esc(t("backup.openHint"))}</p>
      <label class="field"><span>${esc(t("backup.passphrase"))}</span><input name="pass" type="password" autocomplete="off"/></label>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("backup.open"), value: "open", kind: "primary" },
    ],
    onSubmit: (action, root) => {
      value = $<HTMLInputElement>('[name="pass"]', root).value;
      return action !== "open" || value.length > 0;
    },
  });
  return answer === "open" ? value : null;
}

/** Fill and wire the two pages inside the open Settings window. */
export function wireCompanionSettings(root: HTMLElement): void {
  const field = <T extends HTMLElement>(selector: string) => root.querySelector<T>(selector);
  const failed = (error: unknown) => toast("error", t("toast.settingsFailed"), describe(error));

  // Phone remote: saved at once.
  const remote = field<HTMLInputElement>('[name="remoteEnabled"]');
  const showRemote = (view: RemoteView) => {
    showRemoteNote(view);
    if (remote) remote.checked = view.enabled;
    const details = field("[data-remote-details]");
    if (details) details.hidden = !view.enabled;
    const set = (selector: string, text: string) => {
      const element = field(selector);
      if (element) element.textContent = text;
    };
    set("[data-remote-name]", view.name);
    set("[data-remote-ip]", view.address);
    set("[data-remote-pin]", view.pin.replace(/(\d{3})(\d{3})/, "$1 $2"));
    set("[data-remote-paired]", t("remote.paired", { count: view.paired }));
    set("[data-remote-status]", t(view.running ? "remote.running" : "remote.sleeping"));
  };
  const loadRemote = () => void api.remoteInfo().then(showRemote).catch(() => undefined);
  loadRemote();
  remote?.addEventListener("change", () => void api.setRemoteEnabled(remote.checked).then(showRemote).catch(failed));
  field<HTMLButtonElement>("[data-remote-test]")?.addEventListener("click", (event) => {
    const button = event.currentTarget as HTMLButtonElement;
    const result = field("[data-remote-result]");
    button.disabled = true;
    if (result) {
      result.hidden = false;
      result.textContent = t("remote.testing");
    }
    void api
      .remoteTest()
      .then((test) => {
        showRemote(test.view);
        const address = test.view.address.replace(/^http:\/\//, "");
        if (result) result.textContent = !test.page ? t("remote.testFail") : test.name ? t("remote.testOk", { address }) : t("remote.testNoName", { address });
      })
      .catch(failed)
      .finally(() => (button.disabled = false));
  });
  field("[data-remote-forget]")?.addEventListener("click", () => void api.remoteReset(true).then(showRemote).catch(failed));
  // The server starting or stopping while the window is open.
  void events.onRemoteStatus(() => root.isConnected && loadRemote());

  // Auto accept: saved at once.
  const accept = field<HTMLInputElement>('[name="autoAccept"]');
  const acceptDelay = field<HTMLSelectElement>('[name="autoAcceptDelay"]');
  let fillingAccept = false;
  void api
    .autoAcceptPrefs()
    .then((prefs) => {
      fillingAccept = true;
      if (accept) accept.checked = prefs.enabled;
      if (acceptDelay) {
        acceptDelay.value = String(prefs.delaySeconds);
        acceptDelay.dispatchEvent(new Event("change"));
      }
      fillingAccept = false;
    })
    .catch(() => undefined);
  const saveAccept = () => {
    if (fillingAccept || !accept || !acceptDelay) return;
    void api.setAutoAcceptPrefs(accept.checked, Number(acceptDelay.value)).catch(failed);
  };
  accept?.addEventListener("change", saveAccept);
  acceptDelay?.addEventListener("change", saveAccept);

  // Notifications: each switch saves at once.
  const notifyInputs: [keyof NotifyView, string][] = [
    ["decay", "notifyDecay"],
    ["ban", "notifyBan"],
    ["recording", "notifyRecording"],
  ];
  const notifyField = (name: string) => field<HTMLInputElement>(`[name="${name}"]`);
  void api
    .notifyPrefs()
    .then((view) => notifyInputs.forEach(([key, name]) => notifyField(name) && (notifyField(name)!.checked = view[key])))
    .catch(() => undefined);
  notifyInputs.forEach(([, name]) =>
    notifyField(name)?.addEventListener("change", () => {
      const view = Object.fromEntries(notifyInputs.map(([key, input]) => [key, notifyField(input)?.checked ?? true])) as unknown as NotifyView;
      void api.setNotifyPrefs(view).catch(failed);
    }),
  );
  field("[data-notify-test]")?.addEventListener("click", () => {
    sendNotifyTexts();
    void api.testNotification().catch(failed);
  });

  // Backup.
  const enabled = field<HTMLInputElement>('[name="backupEnabled"]');
  const every = field<HTMLSelectElement>('[name="backupEvery"]');
  const keep = field<HTMLSelectElement>('[name="backupKeep"]');
  let backup: BackupView | null = null;
  /** Filling the fields is not the player changing them. */
  let filling = false;
  const show = (view: BackupView) => {
    backup = view;
    filling = true;
    const ready = view.hasPassphrase && view.folder !== "";
    if (enabled) {
      enabled.checked = view.enabled && ready;
      enabled.disabled = !ready;
      enabled.closest(".toggle-row")?.classList.toggle("disabled", !ready);
    }
    if (every) every.value = String(view.everyDays);
    if (keep) keep.value = String(view.keep);
    // The drawn dropdowns read their select again on `change`.
    every?.dispatchEvent(new Event("change"));
    keep?.dispatchEvent(new Event("change"));
    filling = false;
    const folder = field("[data-backup-folder]");
    if (folder) folder.textContent = view.folder || t("backup.folderNone");
    const passphrase = field("[data-backup-passphrase]");
    if (passphrase) passphrase.textContent = t(view.hasPassphrase ? "backup.passphraseSet" : "backup.passphraseNone");
    const setButton = field("[data-backup-set]");
    if (setButton) setButton.textContent = t(view.hasPassphrase ? "backup.passphraseChange" : "backup.passphraseChoose");
    const last = field("[data-backup-last]");
    if (last) {
      const when = view.lastAt ? new Date(view.lastAt).toLocaleString(localeTag(), { dateStyle: "medium", timeStyle: "short" }) : t("backup.never");
      last.textContent = view.lastError ? `${when} · ${errorText(view.lastError)}` : view.files ? `${when} · ${t("backup.files", { count: view.files })}` : when;
    }
    const now = field<HTMLButtonElement>("[data-backup-now]");
    if (now) now.disabled = !ready;
  };
  const save = (change: Partial<BackupView>) => {
    if (!backup) return;
    const next = { ...backup, ...change };
    void api.setBackupPrefs(next.enabled, next.folder, next.everyDays, next.keep).then(show).catch(failed);
  };
  void api.backupPrefs().then(show).catch(() => undefined);
  enabled?.addEventListener("change", () => save({ enabled: enabled.checked }));
  every?.addEventListener("change", () => filling || save({ everyDays: Number(every.value) }));
  keep?.addEventListener("change", () => filling || save({ keep: Number(keep.value) }));
  field("[data-backup-pick]")?.addEventListener("click", () => {
    void api
      .pickBackupFolder(t("backup.choose"))
      .then((folder) => folder && save({ folder }))
      .catch(failed);
  });
  field("[data-backup-set]")?.addEventListener("click", () => {
    void askNewPassphrase().then((passphrase) => {
      if (passphrase === null) return;
      void api
        .setBackupPassphrase(passphrase)
        .then((view) => {
          show(view);
          toast("success", t("backup.passphraseSaved"));
        })
        .catch(failed);
    });
  });
  field("[data-backup-now]")?.addEventListener("click", (event) => {
    const button = event.currentTarget as HTMLButtonElement;
    button.disabled = true;
    void api
      .backupNow()
      .then((path) => toast("success", t("backup.done"), path, 4500))
      .catch((error) => toast("error", t("backup.failed"), describe(error)))
      .finally(() => void api.backupPrefs().then(show).catch(() => undefined));
  });
}

// ---------------------------------------------------------------- overview

const CHAMPION_NAMES = new Map(Object.entries(championIds as Record<string, number>).map(([name, id]) => [id, name]));
const APEX = new Set(["Master", "Grandmaster", "Challenger"]);

function rankText(row: AccountOverview): string {
  const tier = normalizeTier(row.tier);
  if (tier === "Unranked" || tier === "Error") return t("rank.unranked");
  const lp = `${Number.parseInt(row.lp, 10) || 0} LP`;
  return APEX.has(tier) || !row.division ? `${tierName(tier)} · ${lp}` : `${tierName(tier)} ${row.division} · ${lp}`;
}

/** "+24 LP", "−18 LP", "0 LP" or a dash when nothing is known. */
function lpChange(value: number | null): string {
  if (value === null) return `<span class="ov-none">–</span>`;
  const text = `${value > 0 ? "+" : value < 0 ? "−" : ""}${Math.abs(value)} LP`;
  return `<span class="${value > 0 ? "ov-up" : value < 0 ? "ov-down" : "ov-flat"}">${esc(text)}</span>`;
}

function record(wins: number, games: number): string {
  return games ? esc(t("overview.record", { wins, losses: games - wins })) : `<span class="ov-none">–</span>`;
}

function ago(time: number): string {
  if (!time) return t("overview.never");
  const days = Math.floor((Date.now() - time) / DAY);
  const format = new Intl.RelativeTimeFormat(localeTag(), { numeric: "auto" });
  if (days >= 1) return format.format(-days, "day");
  const hours = Math.floor((Date.now() - time) / 3_600_000);
  return hours >= 1 ? format.format(-hours, "hour") : format.format(-Math.max(1, Math.floor((Date.now() - time) / 60_000)), "minute");
}

function championRow(champion: ChampionOverview, most: number): string {
  const name = champion.champion || CHAMPION_NAMES.get(champion.championId) || `#${champion.championId}`;
  const rate = Math.round((champion.wins / Math.max(1, champion.games)) * 100);
  const kda = champion.deaths ? ((champion.kills + champion.assists) / champion.deaths).toFixed(1) : t("overview.perfect");
  return `
    <div class="ov-champ" data-tooltip="${esc(t("overview.champTip", { name, games: champion.games, wins: champion.wins, losses: champion.games - champion.wins, accounts: champion.accounts }))}">
      <img src="${esc(championIcon(champion.championId))}" alt="" width="32" height="32" loading="lazy" draggable="false"/>
      <strong>${esc(name)}</strong>
      <span class="ov-champ-games">${esc(t("overview.games", { count: champion.games }))}</span>
      <span class="ov-bar" style="--share:${(champion.games / Math.max(1, most)) * 100}%"><i style="width:${rate}%"></i></span>
      <span class="ov-champ-rate ${rate >= 50 ? "ov-up" : "ov-down"}">${rate}%</span>
      <span class="ov-champ-kda">${esc(kda)} KDA</span>
    </div>`;
}

type Pool = "7" | "30" | "all";

/** The Overview window. `openAccount` shows an account's page. */
export async function openOverview(openAccount: (key: Key) => void): Promise<void> {
  let pool: Pool = (["7", "30", "all"] as const).find((value) => value === stored("overviewPool", "30")) ?? "30";
  const load = (): Promise<Overview> => {
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    const dayStart = midnight.getTime();
    return api.accountsOverview(dayStart, dayStart - 6 * DAY, pool === "all" ? 0 : Date.now() - Number(pool) * DAY);
  };
  let data: Overview;
  try {
    data = await load();
  } catch (error) {
    toast("error", t("overview.failed"), describe(error));
    return;
  }
  const body = (view: Overview) => {
    const sum = (pick: (row: AccountOverview) => number) => view.accounts.reduce((total, row) => total + pick(row), 0);
    const lpSum = (pick: (row: AccountOverview) => number | null) => (view.accounts.some((row) => pick(row) !== null) ? view.accounts.reduce((total, row) => total + (pick(row) ?? 0), 0) : null);
    const decaying = view.accounts.filter((row) => row.decayLeft !== null).sort((a, b) => (a.decayLeft ?? 0) - (b.decayLeft ?? 0));
    const tile = (label: string, value: string, detail: string) => `<div class="ov-tile"><span>${esc(label)}</span><strong>${value}</strong><em>${detail}</em></div>`;
    const gamesLine = (wins: number, games: number) => (games ? `${record(wins, games)} · ${esc(t("overview.games", { count: games }))}` : esc(t("overview.noGames")));
    const rows = view.accounts
      .map((row) => {
        const decay =
          row.decayLeft === null
            ? `<span class="ov-none">–</span>`
            : `<span class="${row.decayLeft <= 3 ? "ov-down" : row.decayLeft <= 7 ? "ov-warn" : ""}" data-tooltip="${esc(t(row.decayQueue === "RANKED_FLEX_SR" ? "card.decayFlex" : "card.decaySolo"))}">${esc(row.decayLeft <= 0 ? t("card.decaying") : t("card.decay", { days: String(row.decayLeft) }))}</span>`;
        return `
          <tr tabindex="0" role="button" data-ov-account="${esc(row.accountId)}" data-ov-region="${esc(row.region)}">
            <td class="ov-name"><strong>${esc(row.name || row.accountId)}</strong><span>${esc(row.regionDisplay)}</span></td>
            <td class="ov-rank">${emblem(row.tier, 22)}<span>${esc(rankText(row))}</span></td>
            <td>${lpChange(row.lpDay)}<small>${record(row.winsDay, row.gamesDay)}</small></td>
            <td>${lpChange(row.lpWeek)}<small>${record(row.winsWeek, row.gamesWeek)}</small></td>
            <td class="ov-last">${esc(ago(row.lastGame))}</td>
            <td>${decay}</td>
          </tr>`;
      })
      .join("");
    const most = view.champions[0]?.games ?? 0;
    return `
      <div class="ov-tiles">
        ${tile(t("overview.today"), lpChange(lpSum((row) => row.lpDay)), gamesLine(sum((row) => row.winsDay), sum((row) => row.gamesDay)))}
        ${tile(t("overview.week"), lpChange(lpSum((row) => row.lpWeek)), gamesLine(sum((row) => row.winsWeek), sum((row) => row.gamesWeek)))}
        ${tile(
          t("overview.decay"),
          decaying.length ? esc(t("overview.decayCount", { count: decaying.length })) : `<span class="ov-none">–</span>`,
          decaying.length ? esc(t("overview.decaySoonest", { account: decaying[0].name || decaying[0].accountId, days: decaying[0].decayLeft ?? 0 })) : esc(t("overview.decayNone")),
        )}
      </div>
      ${
        view.accounts.length
          ? `<div class="ov-table-wrap"><table class="ov-table">
              <thead><tr><th>${esc(t("overview.account"))}</th><th>${esc(t("overview.rank"))}</th><th>${esc(t("overview.today"))}</th><th>${esc(t("overview.week"))}</th><th>${esc(t("overview.lastGame"))}</th><th>${esc(t("overview.decay"))}</th></tr></thead>
              <tbody>${rows}</tbody>
            </table></div>`
          : `<p class="hint">${esc(t("overview.empty"))}</p>`
      }
      <div class="ov-pool-head">
        <h3>${esc(t("overview.pool"))}</h3>
        <div class="segmented" role="group" aria-label="${esc(t("overview.pool"))}">
          ${(["7", "30", "all"] as const).map((value) => `<button type="button" data-ov-pool="${value}" aria-pressed="${pool === value}">${esc(t(value === "7" ? "overview.pool7" : value === "30" ? "overview.pool30" : "overview.poolAll"))}</button>`).join("")}
        </div>
      </div>
      ${view.champions.length ? `<div class="ov-champs">${view.champions.slice(0, 12).map((champion) => championRow(champion, most)).join("")}</div>` : `<p class="hint">${esc(t("overview.poolEmpty"))}</p>`}
      <p class="settings-note">${esc(t("overview.note"))}</p>`;
  };
  await modal({
    title: t("overview.title"),
    wide: true,
    className: "overview-modal",
    body: `<div class="ov" data-ov>${body(data)}</div>`,
    actions: [{ label: t("common.close"), value: "close" }],
    onOpen: (root) => {
      const box = $<HTMLElement>("[data-ov]", root);
      const pick = (target: HTMLElement) => {
        const row = target.closest<HTMLElement>("[data-ov-account]");
        if (!row) return false;
        root.querySelector<HTMLElement>(".modal-close")?.click();
        openAccount({ accountId: row.dataset.ovAccount ?? "", region: row.dataset.ovRegion ?? "" });
        return true;
      };
      box.addEventListener("click", (event) => {
        const target = event.target as HTMLElement;
        const chosen = target.closest<HTMLElement>("[data-ov-pool]")?.dataset.ovPool as Pool | undefined;
        if (chosen && chosen !== pool) {
          pool = chosen;
          store("overviewPool", pool);
          void load()
            .then((view) => (box.innerHTML = body(view)))
            .catch((error) => toast("error", t("overview.failed"), describe(error)));
          return;
        }
        pick(target);
      });
      box.addEventListener("keydown", (event) => {
        if (event.key === "Enter" && pick(event.target as HTMLElement)) event.preventDefault();
      });
    },
  });
}

// ---------------------------------------------------------------- feature guide

export interface GuideActions {
  recorderOn: () => Promise<boolean>;
  setRecorder: (on: boolean) => Promise<void>;
  musicOn: () => boolean;
  setMusic: (on: boolean) => Promise<void>;
  /** Close the guide and open a Settings page / another window. */
  openSettings: (tab: string) => void;
  openOverview: () => void;
  openGameSettings: () => void;
}

const GUIDE_SEEN = "guideSeen";

/** The guide was not shown yet on this PC. */
export const guideDue = (): boolean => stored(GUIDE_SEEN, "") !== "2";

/** What the app can do besides signing in: each feature with its switch or its door. */
export async function openGuide(actions: GuideActions): Promise<void> {
  store(GUIDE_SEEN, "2");
  const recorder = await actions.recorderOn().catch(() => false);
  const row = (id: string, glyph: string, control: string) => `
    <div class="guide-row">
      <span class="guide-icon">${icon(glyph, 18)}</span>
      <span class="guide-text"><strong>${esc(t(`guide.${id}` as MessageKey))}</strong><span>${esc(t(`guide.${id}Text` as MessageKey))}</span></span>
      ${control}
    </div>`;
  const toggle = (name: string, on: boolean) => `<label class="toggle-row guide-switch"><input type="checkbox" name="${name}" ${on ? "checked" : ""} aria-label="${esc(t("guide.turnOn"))}"/><span class="switch"></span></label>`;
  const door = (target: string, label: MessageKey) => `<button type="button" class="btn btn-ghost" data-guide="${target}">${esc(t(label))}</button>`;
  let next: (() => void) | null = null;
  await modal({
    title: t("guide.title"),
    wide: true,
    className: "guide-modal",
    body: `
      <p class="hint">${esc(t("guide.intro"))}</p>
      <div class="guide-rows">
        ${row("recorder", "video", toggle("guideRecorder", recorder))}
        ${row("overview", "chart-line", door("overview", "guide.open"))}
        ${row("notifications", "monitor", door("notifications", "guide.settings"))}
        ${row("backup", "lock", door("backup", "guide.setUp"))}
        ${row("gameSettings", "sliders", door("game-settings", "guide.open"))}
        ${row("music", "music", toggle("guideMusic", actions.musicOn()))}
      </div>
      <p class="settings-note">${esc(t("guide.again"))}</p>`,
    actions: [{ label: t("guide.done"), value: "close", kind: "primary" }],
    onOpen: (root) => {
      const wireSwitch = (name: string, apply: (on: boolean) => Promise<void>) => {
        const input = root.querySelector<HTMLInputElement>(`[name="${name}"]`);
        input?.addEventListener("change", () => {
          const on = input.checked;
          void apply(on).catch((error) => {
            input.checked = !on;
            toast("error", t("toast.settingsFailed"), describe(error));
          });
        });
      };
      wireSwitch("guideRecorder", actions.setRecorder);
      wireSwitch("guideMusic", actions.setMusic);
      root.addEventListener("click", (event) => {
        const target = (event.target as HTMLElement).closest<HTMLElement>("[data-guide]")?.dataset.guide;
        if (!target) return;
        next = target === "overview" ? actions.openOverview : target === "game-settings" ? actions.openGameSettings : () => actions.openSettings(target);
        root.querySelector<HTMLElement>(".modal-close")?.click();
      });
    },
  });
  (next as (() => void) | null)?.();
}
