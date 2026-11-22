import { api, ApiError } from "./api";
import { esc, modal } from "./dom";
import { loadGameAssets, renderGameInfo } from "./gameinfo";
import { errorText, t } from "./i18n";

/** A game from an account's list as the full end-of-game view (scoreboard, overview, stats, graphs, runes). */
export function openMatchDetails(gameId: number, championId: number, kills: number, deaths: number, assists: number): void {
  void modal({
    title: t("gi.title"),
    className: "match-modal",
    wide: true,
    body: `<div class="match-details"><p class="match-details-note">${esc(t("gi.loading"))}</p></div>`,
    actions: [],
    onOpen: (root) => {
      const box = root.querySelector<HTMLElement>(".match-details");
      if (!box) return;
      void Promise.all([api.matchDetails(gameId, championId, kills, deaths, assists), loadGameAssets()])
        .then(([details]) => {
          if (!box.isConnected) return;
          box.innerHTML = "";
          renderGameInfo(box, details);
        })
        .catch((error: unknown) => {
          if (!box.isConnected) return;
          box.innerHTML = `<p class="match-details-note">${esc(error instanceof ApiError ? errorText(error.code, error.detail) : errorText("game_not_found"))}</p>`;
        });
    },
  });
}
