// Runs inside the app's 3D view on Khada's model viewer page (modelviewer.lol). It does two
// things and never touches the page's own code:
//  1. From the first moment, the app's own loading screen covers the page (the logo's two
//     halves and a running line, as at the app's start), so the site is never seen loading.
//  2. Once the drawing surface exists, everything around it is hidden and it gets the whole
//     view. The loading screen leaves when the model's files have stopped arriving.
// Nothing depends on the page's class names: it works from the largest <canvas>.
(() => {
  if (window.top !== window || window.__lcViewer) return;
  window.__lcViewer = true;
  const KEEP = "data-lc-keep";
  const born = performance.now();

  // This runs when the document is created, before it has any element at all: wait for
  // <html> (it comes at once), then start. Until then the view shows its own dark ground.
  const whenRoot = (run) => {
    if (document.documentElement) return run(document.documentElement);
    const watcher = new MutationObserver(() => {
      if (!document.documentElement) return;
      watcher.disconnect();
      run(document.documentElement);
    });
    watcher.observe(document, { childList: true });
  };

  whenRoot((root) => {

  const style = document.createElement("style");
  style.textContent = `
    html, body { background: #0a0a0c !important; overflow: hidden !important; margin: 0 !important; }
    html.lc-clean body *:not([${KEEP}]):not([${KEEP}="surface"] *) { display: none !important; }
    html.lc-clean [${KEEP}="path"] { position: fixed !important; inset: 0 !important; width: 100vw !important; height: 100vh !important; max-width: none !important; max-height: none !important; min-width: 0 !important; min-height: 0 !important; margin: 0 !important; padding: 0 !important; border: 0 !important; border-radius: 0 !important; transform: none !important; overflow: hidden !important; background: #0a0a0c !important; background-image: none !important; box-shadow: none !important; display: block !important; z-index: 1 !important; }
    html.lc-clean [${KEEP}="surface"] { position: fixed !important; inset: 0 !important; width: 100vw !important; height: 100vh !important; display: block !important; z-index: 2 !important; background: radial-gradient(900px 560px at 50% 38%, rgb(10 132 255 / .10), transparent 70%), #0a0a0c !important; }
    html.lc-clean [${KEEP}="surface"] canvas { width: 100% !important; height: 100% !important; display: block !important; outline: none !important; }
    #lc-wait { position: fixed; inset: 0; z-index: 2147483647; display: grid; place-items: center; background: #0a0a0c; transition: opacity .45s cubic-bezier(.4, 0, .2, 1); }
    #lc-wait.lc-out { opacity: 0; pointer-events: none; }
    #lc-wait .lc-box { position: relative; display: grid; justify-items: center; }
    #lc-wait .lc-glow { position: absolute; left: 50%; top: 48px; width: 520px; height: 520px; margin: -260px 0 0 -260px; border-radius: 50%; background: radial-gradient(closest-side, rgb(10 132 255 / .2), rgb(15 200 214 / .07) 46%, transparent 72%); opacity: 0; animation: lc-glow 1.6s cubic-bezier(.16, 1, .3, 1) .05s forwards; }
    #lc-wait svg { position: relative; display: block; width: 96px; height: 96px; overflow: visible; animation: lc-breathe 2.4s ease-in-out 1.2s infinite; }
    #lc-wait .lc-a, #lc-wait .lc-b { opacity: 0; animation: lc-a 1s cubic-bezier(.16, 1, .3, 1) .1s forwards; }
    #lc-wait .lc-b { animation-name: lc-b; animation-delay: .18s; }
    #lc-wait .lc-line { position: relative; display: block; width: 120px; height: 2px; margin-top: 26px; border-radius: 2px; background: rgb(255 255 255 / .07); overflow: hidden; }
    #lc-wait .lc-line i { display: block; width: 45%; height: 100%; border-radius: 2px; background: linear-gradient(90deg, transparent, #0a84ff, #0fc8d6, transparent); animation: lc-run 1.2s cubic-bezier(.45, 0, .55, 1) infinite; }
    @keyframes lc-glow { to { opacity: 1; } }
    @keyframes lc-a { from { opacity: 0; transform: translate(-240px, -240px); } 55% { opacity: 1; } to { opacity: 1; transform: none; } }
    @keyframes lc-b { from { opacity: 0; transform: translate(240px, 240px); } 55% { opacity: 1; } to { opacity: 1; transform: none; } }
    @keyframes lc-breathe { 50% { transform: scale(1.04); } }
    @keyframes lc-run { from { transform: translateX(-110%); } to { transform: translateX(240%); } }
    @media (prefers-reduced-motion: reduce) { #lc-wait svg, #lc-wait .lc-a, #lc-wait .lc-b, #lc-wait .lc-glow { animation: none; opacity: 1; } #lc-wait .lc-line i { animation: none; width: 100%; } }
  `;
  const wait = document.createElement("div");
  wait.id = "lc-wait";
  wait.setAttribute(KEEP, "ui");
  wait.innerHTML = `
    <div class="lc-box" ${KEEP}="ui">
      <span class="lc-glow" ${KEEP}="ui"></span>
      <svg viewBox="250 254 726 726" ${KEEP}="ui">
        <defs ${KEEP}="ui">
          <linearGradient id="lc-gold" x1="0.35" y1="0" x2="0.15" y2="1" ${KEEP}="ui"><stop offset="0" stop-color="#fbe08e" ${KEEP}="ui"/><stop offset="0.45" stop-color="#ebc56c" ${KEEP}="ui"/><stop offset="1" stop-color="#c38f3c" ${KEEP}="ui"/></linearGradient>
          <linearGradient id="lc-teal" x1="1" y1="0" x2="0" y2="1" ${KEEP}="ui"><stop offset="0" stop-color="#0a9fb2" ${KEEP}="ui"/><stop offset="0.5" stop-color="#0fc8d6" ${KEEP}="ui"/><stop offset="1" stop-color="#079bb0" ${KEEP}="ui"/></linearGradient>
        </defs>
        <path class="lc-a" fill="url(#lc-gold)" ${KEEP}="ui" d="M 933,262 L 425,261 L 257,430 L 258,921 L 390,793 L 390,489 L 465,413 L 786,413 Z"/>
        <path class="lc-b" fill="url(#lc-teal)" ${KEEP}="ui" d="M 967,315 L 822,457 L 822,761 L 738,843 L 415,843 L 287,972 L 809,973 L 969,812 Z"/>
      </svg>
      <span class="lc-line" ${KEEP}="ui"><i ${KEEP}="ui"></i></span>
    </div>`;
  // Before the page has drawn anything: the cover hangs on <html> itself, above everything.
  root.append(style, wait);

  // The model's files are arriving while requests keep finishing.
  let lastArrival = performance.now();
  try {
    new PerformanceObserver(() => { lastArrival = performance.now(); }).observe({ type: "resource", buffered: true });
  } catch { /* old engine: the time limits below still apply */ }

  /** The drawing surface: the biggest canvas on the page. */
  const biggest = () => [...document.querySelectorAll("canvas")]
    .map((canvas) => ({ canvas, area: canvas.clientWidth * canvas.clientHeight }))
    .filter((entry) => entry.area > 40000)
    .sort((a, b) => b.area - a.area)[0]?.canvas;

  let shown = null;
  let isolatedAt = 0;
  let revealed = false;
  const reveal = () => {
    if (revealed) return;
    revealed = true;
    wait.classList.add("lc-out");
    setTimeout(() => wait.remove(), 600);
  };
  const isolate = () => {
    if (!style.isConnected) root.append(style);
    if (!revealed && !wait.isConnected) root.append(wait);
    if (!document.body) return;
    if (!(shown && shown.isConnected)) {
      const canvas = biggest();
      if (canvas) {
        document.querySelectorAll(`[${KEEP}="path"], [${KEEP}="surface"]`).forEach((element) => element.removeAttribute(KEEP));
        // The canvas's own box is the surface; everything above it up to <body> is the path.
        const surface = canvas.parentElement && canvas.parentElement !== document.body ? canvas.parentElement : canvas;
        surface.setAttribute(KEEP, "surface");
        for (let node = surface.parentElement; node && node !== document.body && node !== root; node = node.parentElement) node.setAttribute(KEEP, "path");
        shown = canvas;
        isolatedAt = performance.now();
        root.classList.add("lc-clean");
        // The page sizes its picture when the window changes: tell it that it did.
        requestAnimationFrame(() => window.dispatchEvent(new Event("resize")));
        setTimeout(() => window.dispatchEvent(new Event("resize")), 400);
      }
    }
    if (revealed) return;
    const now = performance.now();
    // Ready: the surface is in place and nothing has arrived for a moment (the model and its
    // textures are in). Never sooner than the opening takes, never later than a fair limit.
    const quiet = now - lastArrival > 1400;
    const ready = isolatedAt && now - isolatedAt > 900 && now - born > 1700 && (quiet || now - isolatedAt > 9000);
    if (ready || now - born > 25000) reveal();
  };
  // Never let one bad round stop the next (the page is not ours and changes under us).
  const round = () => {
    try {
      isolate();
    } catch {
      /* try again at the next round */
    }
  };
  round();
  setInterval(round, 250);
  });
})();
