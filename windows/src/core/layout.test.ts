// node --test — the thin bar must not jump sideways while the island folds into it.

import { test } from "node:test";
import assert from "node:assert/strict";
import { BAR_MAX_W, PANEL_W, islandLeftCss, islandSize, islandX, layoutPrefs } from "./layout.ts";

function withWindowWidth(width: number, fn: () => void) {
  const g = globalThis as { document?: unknown };
  const prev = g.document;
  g.document = { documentElement: { clientWidth: width } };
  try { fn(); } finally { g.document = prev; }
}

/** Evaluates `calc(50% - Npx)` / `Npx` against a window width. */
function resolveLeft(css: string, windowW: number): number {
  const m = /^calc\(50% - (-?[\d.]+)px\)$/.exec(css);
  if (m) return windowW / 2 - Number(m[1]);
  return Number.parseFloat(css);
}

test("folded bar is centred in whatever width the window has", () => {
  Object.assign(layoutPrefs, { bar: true, dock: "top", gap: 0, barStatusW: 0, compactW: 200 });
  for (const windowW of [PANEL_W, BAR_MAX_W, 150, 420]) {
    for (const w of [200, 240, 333]) {
      const left = resolveLeft(islandLeftCss(w), windowW);
      assert.equal(left + w / 2, windowW / 2, `window ${windowW}, bar ${w}`);
    }
  }
});

test("bar centre does not move through the whole fold (panel → bar)", () => {
  Object.assign(layoutPrefs, { bar: true, dock: "top", gap: 0, barStatusW: 0, compactW: 200 });
  const windowW = BAR_MAX_W; // the collapsed window
  for (let w = islandSize("expanded", "overview").w; w >= islandSize("hidden", "overview").w; w -= 7) {
    const left = resolveLeft(islandLeftCss(w), windowW);
    assert.equal(left + w / 2, windowW / 2);
  }
});

test("islandX in bar mode follows the real window width, not the panel's", () => {
  Object.assign(layoutPrefs, { bar: true, dock: "top", gap: 0 });
  withWindowWidth(240, () => assert.equal(islandX(200), 20));
  withWindowWidth(PANEL_W, () => assert.equal(islandX(200), 260));
});

test("without the bar look the island keeps its pixel offset", () => {
  Object.assign(layoutPrefs, { bar: false, dock: "top", gap: 8 });
  assert.equal(islandLeftCss(200), `${(PANEL_W - 200) / 2}px`);
});
