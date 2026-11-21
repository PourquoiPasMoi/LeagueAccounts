// Motion of the Stats tab: panels come in as they scroll into view, bars grow,
// numbers count up, the filter thumbs slide. Everything is CSS driven by a few
// classes set here; nothing runs while the page is still, and nothing moves
// for people who asked Windows for less motion.
import type { MatchRecord, Profile } from "./api";
import { statsContentHtml, statsHtml, trendPanelHtml, type StatsFilters } from "./stats";

/** all: first draw · filters: queue, range or role changed · metric: the trend's
 *  metric changed · data: the account's games were refreshed in the background. */
export type StatsChange = "all" | "filters" | "metric" | "data";

const calm = (): boolean => matchMedia("(prefers-reduced-motion: reduce)").matches;

let watcher: IntersectionObserver | null = null;

/** Count a number up to its value ("52%", "2.18", "51 / 173": the first number moves). */
function countUp(element: HTMLElement, delay: number): void {
  const text = element.textContent ?? "";
  const match = /\d+(?:[.,]\d+)?/.exec(text);
  // Clock times ("26:58") and texts without a number stay as they are.
  if (!match || text.includes(":")) return;
  const raw = match[0];
  const separator = raw.includes(",") ? "," : ".";
  const decimals = raw.includes(separator) && /[.,]/.test(raw) ? raw.length - raw.search(/[.,]/) - 1 : 0;
  const target = Number(raw.replace(",", "."));
  if (!Number.isFinite(target) || target === 0) return;
  const before = text.slice(0, match.index);
  const after = text.slice(match.index + raw.length);
  const show = (value: number) => {
    element.textContent = before + value.toFixed(decimals).replace(".", separator) + after;
  };
  const length = 900;
  let start = 0;
  show(0);
  const step = (now: number) => {
    if (!element.isConnected) return;
    if (!start) start = now + delay;
    const progress = Math.min(1, Math.max(0, (now - start) / length));
    // Fast first, settling at the end.
    show(target * (1 - Math.pow(1 - progress, 4)));
    if (progress < 1) requestAnimationFrame(step);
    else element.textContent = text;
  };
  requestAnimationFrame(step);
}

/** Bring a panel in: its bars and charts follow through CSS, its numbers count. */
function enter(panel: HTMLElement, order: number): void {
  const delay = Math.min(order, 4) * 70;
  panel.style.setProperty("--d", `${delay}ms`);
  panel.classList.add("is-in");
  panel.querySelectorAll<HTMLElement>("[data-count]").forEach((element) => countUp(element, delay + 120));
}

/** Watch the panels inside `root`; each enters when it scrolls into view. */
function reveal(root: HTMLElement, animate: boolean): void {
  const panels = [...root.querySelectorAll<HTMLElement>("[data-reveal]:not(.is-in)")];
  if (!animate || calm()) {
    root.closest(".stats-view")?.classList.add("no-motion");
    panels.forEach((panel) => panel.classList.add("is-in"));
    // Two frames later the instant state is painted: motion may come back for hovers.
    requestAnimationFrame(() => requestAnimationFrame(() => root.closest(".stats-view")?.classList.remove("no-motion")));
    return;
  }
  watcher ??= new IntersectionObserver(
    (entries) => {
      // Panels arriving together come in one after the other, top first.
      const arriving = entries.filter((entry) => entry.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top || a.boundingClientRect.left - b.boundingClientRect.left);
      arriving.forEach((entry, order) => {
        watcher?.unobserve(entry.target);
        enter(entry.target as HTMLElement, order);
      });
    },
    { threshold: 0.12 },
  );
  panels.forEach((panel) => watcher?.observe(panel));
}

/** Slide each filter's thumb under its pressed button. */
function placeThumbs(root: HTMLElement, instant: boolean): void {
  root.querySelectorAll<HTMLElement>(".segmented").forEach((group) => {
    let thumb = group.querySelector<HTMLElement>(":scope > .seg-thumb");
    const fresh = !thumb;
    if (!thumb) {
      thumb = document.createElement("i");
      thumb.className = "seg-thumb";
      thumb.setAttribute("aria-hidden", "true");
      group.prepend(thumb);
      group.classList.add("has-thumb");
    }
    const pressed = group.querySelector<HTMLElement>('button[aria-pressed="true"]');
    if (!pressed) {
      thumb.style.opacity = "0";
      return;
    }
    if (fresh || instant) thumb.style.transition = "none";
    thumb.style.opacity = "1";
    thumb.style.width = `${pressed.offsetWidth}px`;
    thumb.style.transform = `translateX(${pressed.offsetLeft}px)`;
    if (fresh || instant) requestAnimationFrame(() => requestAnimationFrame(() => (thumb.style.transition = "")));
  });
}

/** Draw or update the Stats tab inside the profile page's body. */
export function renderStats(body: HTMLElement, games: MatchRecord[], profile: Profile | null, filters: StatsFilters, change: StatsChange): void {
  const view = body.querySelector<HTMLElement>(":scope > .stats-view");
  const content = view?.querySelector<HTMLElement>(":scope > .stats-content");
  if (!view || !content) {
    body.innerHTML = statsHtml(games, profile, filters);
    const fresh = body.querySelector<HTMLElement>(".stats-view");
    if (fresh) {
      placeThumbs(fresh, true);
      reveal(fresh, true);
    }
    return;
  }
  // The filters stay where they are: only their pressed state moves.
  view.querySelectorAll<HTMLElement>(":scope > .stats-toolbar button").forEach((button) => {
    const { statsQueue, statsRange } = button.dataset;
    if (statsQueue !== undefined) button.setAttribute("aria-pressed", String(statsQueue === filters.queue));
    if (statsRange !== undefined) button.setAttribute("aria-pressed", String(Number(statsRange) === filters.range));
  });
  const trend = content.querySelector<HTMLElement>(".trend-panel");
  if (change === "metric" && trend) {
    // Only the chart changes: the panel stays, its thumb slides, the new bars rise.
    const next = document.createElement("div");
    next.innerHTML = trendPanelHtml(games, profile, filters);
    const chart = next.querySelector(".trend-panel");
    if (chart) {
      trend.querySelectorAll<HTMLElement>("[data-stats-metric]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.statsMetric === filters.metric)));
      [...trend.children].forEach((child) => child.classList.contains("panel-head") || child.remove());
      [...chart.children].forEach((child) => child.classList.contains("panel-head") || trend.append(child));
      // The new chart animates on its own (CSS keyframes start when it is put in).
      trend.style.setProperty("--d", "0ms");
    }
    placeThumbs(view, false);
    return;
  }
  const scroll = body.scrollTop;
  content.innerHTML = statsContentHtml(games, profile, filters);
  body.scrollTop = scroll;
  placeThumbs(view, false);
  reveal(view, change !== "data");
}

/** all: first draw or tab change · chart: only the rank chart changed (zoom,
 *  queue) · data: the account was refreshed in the background. */
export type OverviewChange = "all" | "chart" | "data";

/** Draw or update the Overview tab from its HTML. */
export function renderOverview(body: HTMLElement, html: string, change: OverviewChange): void {
  const view = body.querySelector<HTMLElement>(":scope > .overview-view");
  if (change === "chart" && view) {
    const next = document.createElement("div");
    next.innerHTML = html;
    // The rank boxes stay: only which one is shown moves.
    const cards = view.querySelectorAll<HTMLElement>(".ranked-card");
    next.querySelectorAll<HTMLElement>(".ranked-card").forEach((card, index) => {
      cards[index]?.classList.toggle("is-shown", card.classList.contains("is-shown"));
      cards[index]?.setAttribute("aria-pressed", card.getAttribute("aria-pressed") ?? "false");
    });
    const panel = view.querySelector<HTMLElement>(".rank-panel");
    const fresh = next.querySelector<HTMLElement>(".rank-panel");
    if (panel && fresh && cards.length === next.querySelectorAll(".ranked-card").length) {
      const title = panel.querySelector(".panel-head h3");
      const freshTitle = fresh.querySelector(".panel-head h3");
      if (title && freshTitle) title.innerHTML = freshTitle.innerHTML;
      const pressed = new Set([...fresh.querySelectorAll<HTMLElement>('[data-chart-zoom][aria-pressed="true"]')].map((button) => button.dataset.chartZoom));
      panel.querySelectorAll<HTMLElement>("[data-chart-zoom]").forEach((button) => button.setAttribute("aria-pressed", String(pressed.has(button.dataset.chartZoom))));
      // The new chart draws itself (CSS keyframes start when it is put in).
      [...panel.children].forEach((child) => child.classList.contains("panel-head") || child.remove());
      [...fresh.children].forEach((child) => child.classList.contains("panel-head") || panel.append(child));
      panel.style.setProperty("--d", "0ms");
      placeThumbs(view, false);
      return;
    }
  }
  const scroll = body.scrollTop;
  body.innerHTML = html;
  body.scrollTop = scroll;
  const fresh = body.querySelector<HTMLElement>(":scope > .overview-view");
  if (!fresh) return;
  placeThumbs(fresh, true);
  reveal(fresh, change !== "data");
}
