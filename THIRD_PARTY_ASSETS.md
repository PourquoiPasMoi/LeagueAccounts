# Third-party assets

## League client art



The artwork, fonts and animations under `ui/public/league/` are Riot Games assets, distributed through CommunityDragon's patch **16.20** client archive. They are not covered by this repository's MIT license. Riot Games, Inc. retains all rights to these materials.

`ui/public/league/manifest.json` records the original URL, byte size and SHA-256 of every bundled file. The interface loads these files locally; it makes no runtime requests to CommunityDragon. No sound or video files are included.

The bundle contains:

- League ranked emblems and LoL/TFT miniature crests from `rcp-fe-lol-static-assets`.
- Full-size unranked art from `rcp-fe-lol-shared-components/unranked.png`, referenced by the client's `lol-regalia-emblem-element` (`--regalia-emblem-unranked`). Cards and the login view use this emblem; sidebar filters retain the native miniature crests. Its transparent canvas padding is accounted for in CSS.
- Beaufort for LoL and Spiegel fonts from `rcp-fe-lol-typekit`.
- Client backgrounds, navigation art and the empty-search Poro from `rcp-fe-lol-static-assets`, plus the TFT game selector from `rcp-fe-lol-parties`. Rank crests are static, including on hover.
- Dialog trim, the close-button mount and icon, and the title divider from `rcp-fe-lol-uikit`. The modal frame follows the live client's `lol-uikit-dialog-frame` construction, inspected in Customize My Identity.
- Checkbox sprites, dropdown arrows and checks, navigation highlights, action icons, and tooltip caret from `rcp-fe-lol-uikit`; radio sprites from `rcp-fe-lol-parties`; the loading spinner and utility icons from `rcp-fe-lol-static-assets`; window controls from `rcp-fe-lol-navigation`.

Since v5.0.0 the interface has its own design (CSS-drawn controls; its font is listed below). The client's fonts, frames, control sprites and backgrounds listed above are still in the bundle and the manifest but are no longer used by the stylesheets; rank emblems, crests and the small glyph icons are.

Verify the committed bundle with `npm run assets:verify`. Restore missing files with `node scripts/league-assets.mjs --fetch`. Existing valid files are retained. An upstream hash mismatch requires manual review before changing the manifest; the CDN may re-encode PNGs, so a different hash does not necessarily mean different artwork. To update the patch, change the script's source version, inspect the resulting art and motion, and deliberately regenerate the manifest and showcase.

## Interface font

The interface uses **Chillax** by Manushi Parikh, from the Indian Type Foundry's [Fontshare](https://www.fontshare.com/fonts/chillax), free for personal and commercial use under the ITF Free Font License. It is bundled as one variable file, `ui/public/fonts/Chillax-Variable.woff2` (weights 200–700), so the interface makes no font requests at runtime. Chinese text falls back to the CJK fonts below.

## CJK fonts

Chinese text needs glyphs the League typekit does not ship. Two open-source families cover it, both under the [SIL Open Font License 1.1](https://openfontlicense.org), which permits bundling and subsetting:

- **Noto Sans SC** (`noto-sans-sc-{regular,medium,bold}.otf`) — the interface face. A sans-serif matches the weight of the Latin text at 12–13px.
- **LXGW WenKai** (霞鹜文楷, `lxgw-wenkai-{regular,medium}.ttf`) — the display face, for the Beaufort-stacked headings where its handwritten strokes are legible and distinctive. It ships no Bold, so it is declared for 400–500 and 600–900 separately.

Neither is a system font: relying on the OS would change the interface per machine, and shipping a Windows font would be a licensing problem.

`node scripts/cjk-font.mjs` generates both families, subset to the characters the interface can actually render — 764 of each font's ~20,000 — which keeps all five files near 1.2 MB instead of 70 MB. Re-run it after changing any UI string. Sources are read from the Windows font directories rather than vendored; a missing one is reported and skipped.

`npm run assets:verify` does not cover these files: `ui/public/league/manifest.json` records Riot's assets and their upstream URLs, and these fonts have neither.

League Accounts is an independent community project, not endorsed by Riot Games. Riot Games, League of Legends and Teamfight Tactics are trademarks or registered trademarks of Riot Games, Inc.
