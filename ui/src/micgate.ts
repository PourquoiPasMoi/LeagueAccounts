// Microphone window (Settings › Recorder › Microphone › Set up): which
// microphone is recorded, its live level, and its own noise gate (the line on
// the meter, hold and fade times, listen to myself, measure my typing). Every
// microphone keeps its own gate, by Windows endpoint id. Everything is saved at
// once and heard at once, also by a game being recorded.

import { api, events, type GateLevel, type GateSettings } from "./api";
import { esc, modal } from "./dom";
import { t } from "./i18n";

export const GATE_DEFAULT: GateSettings = { enabled: false, thresholdDb: -42, holdMs: 200, releaseMs: 150 };

const FLOOR = -80;

export interface Microphone {
  id: string;
  name: string;
  console: boolean;
  communications: boolean;
}

export interface MicState {
  /** Saved choice; empty = Windows' default communication device. */
  device: string;
  gates: Record<string, GateSettings>;
}

/** The microphone actually used for a saved choice (the default resolved like the recorder does). */
export function resolveMic(device: string, list: Microphone[]): Microphone | undefined {
  if (device) return list.find((mic) => mic.id === device);
  return list.find((mic) => mic.communications) ?? list.find((mic) => mic.console);
}

/** One line for Settings: the microphone and its gate. */
export function micSummary(state: MicState, list: Microphone[]): string {
  const mic = resolveMic(state.device, list);
  const gate = (mic && state.gates[mic.id]) || GATE_DEFAULT;
  const gateText = gate.enabled ? t("gate.onAt", { db: String(Math.round(gate.thresholdDb)) }) : t("gate.offShort");
  return mic ? t("mic.summary", { name: mic.name, gate: gateText }) : t("mic.none");
}

const share = (db: number) => Math.min(1, Math.max(0, (db - FLOOR) / -FLOOR));

function slider(name: string, label: string, hint: string, min: number, max: number, step: number, value: number): string {
  return `<label class="gate-slider">
    <span class="toggle-text"><strong>${esc(label)}</strong><span>${esc(hint)}</span></span>
    <input type="range" name="${name}" min="${min}" max="${max}" step="${step}" value="${value}"/>
    <output data-gate-out="${name}">${value} ms</output>
  </label>`;
}

function options(device: string, list: Microphone[]): string {
  const fallback = resolveMic("", list);
  return (
    `<option value="">${esc(fallback ? t("settings.recMicDefaultNamed", { name: fallback.name }) : t("settings.recMicDefault"))}</option>` +
    list.map((mic) => `<option value="${esc(mic.id)}"${mic.id === device ? " selected" : ""}>${esc(mic.name)}</option>`).join("")
  );
}

function body(state: MicState, list: Microphone[], gate: GateSettings): string {
  return `<div class="gate">
    <section class="gate-section">
      <label class="field"><span>${esc(t("mic.device"))}</span>
        <div class="select-wrap"><select name="micDevice">${options(state.device, list)}</select></div></label>
      <div class="gate-head">
        <span class="toggle-text"><strong>${esc(t("gate.level"))}</strong><span>${esc(t("gate.levelHint"))}</span></span>
        <span class="gate-state" data-gate-state>${esc(t("gate.closed"))}</span>
      </div>
      <div class="gate-meter" data-gate-meter style="--in: 0; --out: 0; --line: ${share(gate.thresholdDb)}">
        <span class="gate-meter-in"></span>
        <span class="gate-meter-out"></span>
        <input type="range" name="gateThreshold" min="${FLOOR}" max="0" step="1" value="${Math.round(gate.thresholdDb)}" aria-label="${esc(t("gate.threshold"))}"/>
      </div>
      <div class="gate-scale"><span>${FLOOR}</span><span>-60</span><span>-40</span><span>-20</span><span>0 dB</span></div>
      <p class="gate-note" data-gate-note hidden></p>
    </section>
    <section class="gate-section">
      <label class="toggle-row">
        <span class="toggle-text"><strong>${esc(t("gate.forMic"))}</strong><span>${esc(t("gate.forMicHint"))}</span></span>
        <input type="checkbox" name="gateOn" ${gate.enabled ? "checked" : ""}/>
        <span class="switch"></span>
      </label>
      <p class="gate-line"><span>${esc(t("gate.threshold"))}</span><strong data-gate-db>${Math.round(gate.thresholdDb)} dB</strong></p>
      ${slider("gateHold", t("gate.hold"), t("gate.holdHint"), 0, 1000, 10, gate.holdMs)}
      ${slider("gateRelease", t("gate.release"), t("gate.releaseHint"), 20, 1000, 10, gate.releaseMs)}
      <div class="gate-actions">
        <button type="button" class="btn btn-ghost" data-gate-measure>${esc(t("gate.measure"))}</button>
        <button type="button" class="btn btn-ghost" data-gate-reset>${esc(t("gate.reset"))}</button>
        <span class="gate-actions-hint">${esc(t("gate.measureHint"))}</span>
      </div>
    </section>
    <section class="gate-section">
      <label class="toggle-row">
        <span class="toggle-text"><strong>${esc(t("gate.listen"))}</strong><span>${esc(t("gate.listenHint"))}</span></span>
        <input type="checkbox" name="gateListen"/>
        <span class="switch"></span>
      </label>
    </section>
  </div>`;
}

/** The microphone window. Returns the choice and the gates as saved. */
export async function openMicrophone(start: MicState, list: Microphone[]): Promise<MicState> {
  const state: MicState = { device: start.device, gates: { ...start.gates } };
  let mic = resolveMic(state.device, list);
  let gate: GateSettings = { ...((mic && state.gates[mic.id]) || GATE_DEFAULT) };
  let listen = false;
  let saveTimer = 0;
  let saving: Promise<unknown> = Promise.resolve();
  const saveNow = () => {
    window.clearTimeout(saveTimer);
    saveTimer = 0;
    if (!mic) return;
    const id = mic.id;
    const value = { ...gate };
    state.gates[id] = value;
    saving = api.setMicGate(id, value).catch(() => undefined);
  };
  const save = () => {
    window.clearTimeout(saveTimer);
    saveTimer = window.setTimeout(saveNow, 250);
  };
  let stopEvents: (() => void) | undefined;
  let closed = false;
  let ping = 0;
  let measuring: { until: number; loudest: number } | null = null;
  let silentSince = performance.now();

  await modal({
    title: t("mic.title"),
    body: body(state, list, gate),
    actions: [{ label: t("common.close"), kind: "primary", value: "close" }],
    className: "gate-modal",
    onOpen: (root) => {
      const meter = root.querySelector<HTMLElement>("[data-gate-meter]")!;
      const stateLabel = root.querySelector<HTMLElement>("[data-gate-state]")!;
      const note = root.querySelector<HTMLElement>("[data-gate-note]")!;
      const field = (name: string) => root.querySelector<HTMLInputElement>(`[name="${name}"]`)!;
      const say = (text: string) => {
        note.hidden = !text;
        note.textContent = text;
      };
      const show = () => {
        meter.style.setProperty("--line", String(share(gate.thresholdDb)));
        root.querySelector("[data-gate-db]")!.textContent = `${Math.round(gate.thresholdDb)} dB`;
        root.querySelector('[data-gate-out="gateHold"]')!.textContent = `${gate.holdMs} ms`;
        root.querySelector('[data-gate-out="gateRelease"]')!.textContent = `${gate.releaseMs} ms`;
        meter.classList.toggle("is-off", !gate.enabled);
      };
      const fill = () => {
        field("gateOn").checked = gate.enabled;
        field("gateThreshold").value = String(Math.round(gate.thresholdDb));
        field("gateHold").value = String(gate.holdMs);
        field("gateRelease").value = String(gate.releaseMs);
        show();
      };
      const monitor = () => {
        silentSince = performance.now();
        void api.startMicMonitor(state.device, listen).catch(() => say(t("gate.failed")));
      };
      show();
      root.addEventListener("input", (event) => {
        const target = event.target as HTMLInputElement;
        if (target.name === "gateThreshold") gate.thresholdDb = Number(target.value);
        else if (target.name === "gateHold") gate.holdMs = Number(target.value);
        else if (target.name === "gateRelease") gate.releaseMs = Number(target.value);
        else return;
        show();
        save();
      });
      root.addEventListener("change", (event) => {
        const target = event.target as HTMLInputElement | HTMLSelectElement;
        if (target.name === "gateOn") {
          gate.enabled = (target as HTMLInputElement).checked;
          show();
          save();
        }
        if (target.name === "gateListen") {
          listen = (target as HTMLInputElement).checked;
          void api.micMonitorPing(listen);
        }
        if (target.name === "micDevice") {
          // The gate being edited belongs to the old microphone: saved first.
          if (saveTimer) saveNow();
          state.device = target.value;
          void api.setMicDevice(state.device).catch(() => undefined);
          mic = resolveMic(state.device, list);
          gate = { ...((mic && state.gates[mic.id]) || GATE_DEFAULT) };
          measuring = null;
          say("");
          fill();
          monitor();
        }
      });
      root.addEventListener("click", (event) => {
        const button = event.target as HTMLElement;
        if (button.closest("[data-gate-reset]")) {
          gate = { ...GATE_DEFAULT, thresholdDb: gate.thresholdDb, enabled: gate.enabled };
          fill();
          save();
        }
        if (button.closest("[data-gate-measure]") && !measuring) {
          measuring = { until: performance.now() + 3000, loudest: -100 };
          say(t("gate.measuring", { s: "3" }));
        }
      });
      // Shown level: jumps up, falls back smoothly.
      let shownIn = -100;
      let shownOut = -100;
      void events
        .onMicGateLevel((level: GateLevel) => {
          if (level.failed) {
            say(t("gate.failed"));
            return;
          }
          shownIn = Math.max(level.input, shownIn - 3);
          shownOut = Math.max(level.output, shownOut - 3);
          meter.style.setProperty("--in", String(share(shownIn)));
          meter.style.setProperty("--out", String(share(shownOut)));
          const open = gate.enabled ? level.open : true;
          stateLabel.textContent = gate.enabled ? (open ? t("gate.open") : t("gate.closed")) : t("gate.offShort");
          stateLabel.classList.toggle("is-open", gate.enabled && open);
          // A microphone sending pure silence (muted or switched off): said after 3 s.
          if (level.input > -99) {
            silentSince = performance.now();
            if (note.textContent === t("mic.silent")) say("");
          } else if (performance.now() - silentSince > 3000 && !measuring && note.hidden) {
            say(t("mic.silent"));
          }
          if (measuring) {
            measuring.loudest = Math.max(measuring.loudest, level.input);
            const left = Math.ceil((measuring.until - performance.now()) / 1000);
            if (left > 0) {
              say(t("gate.measuring", { s: String(left) }));
            } else {
              // Just above the loudest typing.
              gate.thresholdDb = Math.min(-6, Math.max(FLOOR, Math.round(measuring.loudest + 5)));
              gate.enabled = true;
              measuring = null;
              say(t("gate.measured", { db: String(gate.thresholdDb) }));
              fill();
              save();
            }
          }
        })
        .then((stop) => {
          if (closed) stop();
          else stopEvents = stop;
        });
      monitor();
      ping = window.setInterval(() => void api.micMonitorPing(listen).catch(() => undefined), 2000);
    },
  });
  closed = true;
  window.clearInterval(ping);
  stopEvents?.();
  void api.stopMicMonitor().catch(() => undefined);
  if (saveTimer) saveNow();
  await saving;
  return state;
}
