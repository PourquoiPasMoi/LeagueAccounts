// Regenerates ui/src/data/champion-meta.json: each champion's class (Riot's
// first tag: Mage, Fighter, Tank, Assassin, Marksman, Support) and the lanes it
// is played in. The Stats tab uses it for "Preferred classes" and "Position
// champion coverage". Sources: Data Dragon (classes, ~30 KB compressed) and
// Meraki Analytics (positions, ~1.2 MB compressed). Run after new champions:
//
//   node scripts/champion-meta.mjs                          # download both
//   node scripts/champion-meta.mjs champion.json meraki.json # use files already downloaded
//
// Output: { "<championId>": { "c": "Mage", "p": ["MIDDLE"] } }
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(root, "ui/src/data/champion-meta.json");
const [ddragonFile, merakiFile] = process.argv.slice(2);

const download = async (url) => {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`download failed: HTTP ${response.status} ${url}`);
  return response.json();
};
const local = async (file) => JSON.parse(await readFile(file, "utf8"));

const ddragon = ddragonFile
  ? await local(ddragonFile)
  : await download(`https://ddragon.leagueoflegends.com/cdn/${(await download("https://ddragon.leagueoflegends.com/api/versions.json"))[0]}/data/en_US/champion.json`);
const meraki = merakiFile
  ? await local(merakiFile)
  : await download("https://cdn.merakianalytics.com/riot/lol/resources/latest/en-US/champions.json");

const LANES = { TOP: "TOP", JUNGLE: "JUNGLE", MIDDLE: "MIDDLE", BOTTOM: "BOTTOM", SUPPORT: "UTILITY" };
const positions = {};
for (const champion of Object.values(meraki)) {
  positions[champion.id] = (champion.positions ?? []).map((lane) => LANES[lane]).filter(Boolean);
}

const meta = {};
for (const champion of Object.values(ddragon.data)) {
  const id = Number(champion.key);
  meta[id] = { c: champion.tags?.[0] ?? "", p: positions[id] ?? [] };
}
await writeFile(output, `${JSON.stringify(meta)}\n`);
console.log(`${Object.keys(meta).length} champions → ${output}`);
