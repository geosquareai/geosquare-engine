// Zoom rollup: expected values are worked out by hand from the fixture below. Each rule must
// match the DuckDB rollup (bindings/duckdb/src/dataset.rs) on the same rows, NULLs included.
import { test } from "node:test";
import assert from "node:assert/strict";
import { gs } from "./load.mjs";
import { levelForZoom, rollupToLevel } from "../js/rollup.mjs";

const PARENT = gs.encode(10, 300, 300); // level 10; its children are level 11 (25 cells)
const [c0, c1, c2, c3, c4] = gs.children(PARENT);

// Five child rows. c2 has no value, c3 has no weight. Weight is `w`, category is `cat`.
const rows = [
  { gid: c0, v: 10, w: 2, cat: 1 },
  { gid: c1, v: 20, w: 3, cat: 2 },
  { gid: c2, v: null, w: 5, cat: 2 },
  { gid: c3, v: 40, w: null, cat: 1 },
  { gid: c4, v: 50, w: 1, cat: 1 },
];

test("sum skips NULL values and takes no weight", () => {
  const [out] = rollupToLevel(rows, 10, { v: { rule: "sum" } });
  assert.deepEqual(out, { gid: PARENT, v: 120 });
});

test("weighted_mean skips rows with NULL value or NULL weight", () => {
  // (10*2 + 20*3 + 50*1) / (2 + 3 + 1) = 130 / 6
  const [out] = rollupToLevel(rows, 10, { v: { rule: "weighted_mean", weight: "w" } });
  assert.ok(Math.abs(out.v - 130 / 6) < 1e-12);
});

test("weighted_mean with weight area is the plain mean of the values", () => {
  // Every cell at one level has the same area, so the weight is 1 for each.
  const [out] = rollupToLevel(rows, 10, { v: { rule: "weighted_mean", weight: "area" } });
  assert.equal(out.v, 120 / 4);
});

test("median of an even count is the mean of the two middle values", () => {
  // Values 10, 20, 40, 50: middle two are 20 and 40.
  const [out] = rollupToLevel(rows, 10, { v: { rule: "median" } });
  assert.equal(out.v, 30);
});

test("max and min skip NULL values", () => {
  const [out] = rollupToLevel(rows, 10, { v: { rule: "max" }, cat: { rule: "min" } });
  assert.equal(out.v, 50);
  assert.equal(out.cat, 1);
});

test("majority_weighted picks the category with the largest summed weight", () => {
  // cat 1: weight 2 + 1 = 3. cat 2: weight 3 + 5 = 8. c3 is skipped (NULL weight).
  const [out] = rollupToLevel(rows, 10, { cat: { rule: "majority_weighted", weight: "w" } });
  assert.equal(out.cat, 2);
});

test("majority_weighted returns NULL on a tie", () => {
  const tied = [
    { gid: c0, cat: 1, w: 1 },
    { gid: c1, cat: 2, w: 1 },
  ];
  const [out] = rollupToLevel(tied, 10, { cat: { rule: "majority_weighted", weight: "w" } });
  assert.equal(out.cat, null);
});

test("a parent with one child returns that child's value, and median is NULL", () => {
  const one = [{ gid: c0, v: 10, w: 2 }];
  const [out] = rollupToLevel(one, 10, {
    v: { rule: "sum" },
    m: { rule: "weighted_mean", weight: "w" },
  });
  assert.deepEqual(out, { gid: PARENT, v: 10, m: null });
  const [med] = rollupToLevel(one, 10, { v: { rule: "median" } });
  assert.equal(med.v, null);
});

test("a parent whose values are all NULL returns NULL", () => {
  const empty = [
    { gid: c0, v: null, w: 1 },
    { gid: c1, v: undefined, w: 1 },
  ];
  const [out] = rollupToLevel(empty, 10, { v: { rule: "sum" }, m: { rule: "max" } });
  assert.deepEqual(out, { gid: PARENT, v: null, m: null });
});

test("rows two levels down are climbed to the target level", () => {
  // Level 12 cells under c0. Values 1, 2, 3, 4: median of four is (2 + 3) / 2.
  const grand = gs.children(c0);
  const deep = grand.map((gid, i) => ({ gid, v: i + 1 }));
  const [sum] = rollupToLevel(deep, 10, { v: { rule: "sum" } });
  assert.deepEqual(sum, { gid: PARENT, v: 10 });
  const [median] = rollupToLevel(deep, 10, { v: { rule: "median" } });
  assert.equal(median.v, 2.5);
});

test("output has one row per parent, sorted by GID", () => {
  const other = gs.encode(10, 301, 300);
  const rowsOther = gs.children(other).slice(0, 3).map((gid) => ({ gid, v: 1 }));
  const out = rollupToLevel([...rows.slice(0, 2), ...rowsOther], 10, { v: { rule: "sum" } });
  assert.deepEqual(
    out.map((row) => row.gid),
    [PARENT, other].sort(),
  );
  assert.equal(out.find((row) => row.gid === other).v, 3);
});

test("a level finer than the row level throws", () => {
  assert.throws(() => rollupToLevel(rows, 12, { v: { rule: "sum" } }), /finer than the row level/);
});

test("bad rules and values throw instead of returning a number", () => {
  assert.throws(() => rollupToLevel(rows, 10, { v: { rule: "mean" } }), /unknown rule/);
  assert.throws(() => rollupToLevel(rows, 10, { v: { rule: "weighted_mean" } }), /needs a weight/);
  assert.throws(() => rollupToLevel(rows, 10, { v: { rule: "sum", weight: "w" } }), /takes no weight/);
  assert.throws(() => rollupToLevel(rows, 10, { gid: { rule: "sum" } }), /cannot be a value column/);
  const bad = [{ gid: c0, v: NaN, w: 1 }];
  assert.throws(() => rollupToLevel(bad, 10, { v: { rule: "sum" } }), /finite numbers or null/);
});

test("levelForZoom maps zoom to level 12, 11, 10 or 9", () => {
  assert.deepEqual([16, 15, 14, 13, 12, 11, 3].map(levelForZoom), [12, 12, 11, 10, 10, 9, 9]);
  assert.throws(() => levelForZoom(NaN), /finite number/);
});
