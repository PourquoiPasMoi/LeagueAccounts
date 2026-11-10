// Regenerates ui/src/data/skin-prices.json (and champion-ids.json), the RP price table the account
// value calculator uses. Source: Meraki Analytics' community champion data
// (about 1.2 MB compressed). Run after new skins come out:
//
//   node scripts/skin-prices.mjs                 # download the latest data
//   node scripts/skin-prices.mjs champions.json  # use a file already downloaded
//
// Output shape (kept small: it ships inside the app):
//   { updated: "YYYY-MM-DD",
//     skins: { "<skinId>": <RP> | "special" },   // special = never sold for RP
//     champions: { "<championId>": <RP> } }      // the champion's own RP price
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SOURCE = "https://cdn.merakianalytics.com/riot/lol/resources/latest/en-US/champions.json";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(root, "ui/src/data/skin-prices.json");

const local = process.argv[2];
const data = local
  ? JSON.parse(await readFile(local, "utf8"))
  : await fetch(SOURCE).then((response) => {
      if (!response.ok) throw new Error(`download failed: HTTP ${response.status}`);
      return response.json();
    });

const skins = {};
const champions = {};
for (const champion of Object.values(data)) {
  for (const skin of champion.skins ?? []) {
    const cost = Number.isInteger(skin.cost) && skin.cost > 0 ? skin.cost : "special";
    if (skin.isBase) {
      if (typeof cost === "number") champions[champion.id] = cost;
      continue;
    }
    skins[skin.id] = cost;
  }
}

await mkdir(dirname(output), { recursive: true });
// Champion name → id, for naming OP.GG games (which only give the name).
const ids = Object.fromEntries(Object.values(data).map((champion) => [champion.name, champion.id]));
await writeFile(resolve(root, "ui/src/data/champion-ids.json"), JSON.stringify(ids) + "\n");
const table = { updated: new Date().toISOString().slice(0, 10), skins, champions };
await writeFile(output, JSON.stringify(table) + "\n");
console.log(`${Object.keys(skins).length} skins, ${Object.keys(champions).length} champions → ${output}`);
