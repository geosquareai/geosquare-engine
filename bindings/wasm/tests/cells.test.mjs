// GeoSquareLayer data: every polygon must equal the core ring for its GID, and zoom rollup
// must match rollupToLevel. Uses the wasm core, so no browser is needed.
import { test } from "node:test";
import assert from "node:assert/strict";
import { gs } from "./load.mjs";
import { cellPolygons } from "../js/cells.mjs";
import { GeoSquareLayer } from "../js/deck.mjs";

const PARENT = gs.encode(10, 300, 300);
const kids = gs.children(PARENT);
const rows = kids.slice(0, 4).map((gid, i) => ({ gid, value: [10, 20, null, 40][i] }));
const byGid = (x) => (d) => d.gid;
const valueOf = (d) => d.value;

test("each polygon equals the core ring for its GID", () => {
  const out = cellPolygons(rows, { domainId: 1, getGid: byGid(), getValue: valueOf, maxSegmentLengthM: 25000 });
  assert.equal(out.length, rows.length);
  for (const [i, cell] of out.entries()) {
    assert.equal(cell.gid, rows[i].gid);
    assert.equal(cell.value, rows[i].value);
    assert.deepEqual(cell.polygon, gs.cellPolygonWgs84(1, rows[i].gid, 25000, false));
  }
});

test("zoom rollup folds children into the parent cell with the given rule", () => {
  const out = cellPolygons(rows, {
    domainId: 1,
    getGid: byGid(),
    getValue: valueOf,
    zoom: 12, // levelForZoom(12) is 10, the parent level
    aggregation: { rule: "sum" },
    maxSegmentLengthM: 25000,
  });
  assert.equal(out.length, 1);
  assert.equal(out[0].gid, PARENT);
  assert.equal(out[0].value, 70); // 10 + 20 + 40, the NULL is skipped
  assert.deepEqual(out[0].polygon, gs.cellPolygonWgs84(1, PARENT, 25000, false));
});

test("weighted rollup reads the weight column from the rows", () => {
  // Values 10, 20, NULL, 40 with weights 1, 3, 5, 7. The NULL row is skipped, with its weight.
  // (10*1 + 20*3 + 40*7) / (1 + 3 + 7) = 350 / 11
  const weighted = kids.slice(0, 4).map((gid, i) => ({
    gid,
    value: [10, 20, null, 40][i],
    population: [1, 3, 5, 7][i],
  }));
  const [cell] = cellPolygons(weighted, {
    domainId: 1,
    getGid: byGid(),
    getValue: valueOf,
    zoom: 12,
    aggregation: { rule: "weighted_mean", weight: "population" },
    maxSegmentLengthM: 25000,
  });
  assert.ok(Math.abs(cell.value - 350 / 11) < 1e-12, `got ${cell.value}`);
});

test("zoom and aggregation must come together", () => {
  assert.throws(() => cellPolygons(rows, { domainId: 1, getGid: byGid(), getValue: valueOf, zoom: 12 }), /set together/);
});

test("domainId is required", () => {
  assert.throws(() => cellPolygons(rows, { getGid: byGid(), getValue: valueOf }), /domainId/);
});

test("GeoSquareLayer builds one polygon layer with the cell rings", () => {
  const layer = new GeoSquareLayer({ id: "cells", data: rows, domainId: 1, getValue: valueOf });
  layer.internalState = { isAsyncPropLoading: () => false };
  const sub = layer.renderLayers();
  assert.equal(sub.constructor.layerName, "PolygonLayer");
  assert.equal(sub.props.data.length, rows.length);
  assert.deepEqual(sub.props.getPolygon(sub.props.data[0]), gs.cellPolygonWgs84(1, rows[0].gid, 25000, false));
});
