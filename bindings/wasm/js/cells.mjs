// Turns GID rows into WGS84 cell rings for a map. The rings come from the Rust core
// (`cellPolygonWgs84`). Optional zoom rollup uses `rollupToLevel` from ./rollup.mjs.
import { cellPolygonWgs84, decode } from "../pkg/geosquare_wasm.js";
import { levelForZoom, rollupToLevel } from "./rollup.mjs";

/**
 * Returns `[{ polygon, value, gid }]`, one item per cell. `polygon` is the closed ring of
 * `[lon, lat]` pairs from the core.
 *
 * - `rows`: any array. `getGid(row)` gives the GID, `getValue(row)` gives the number or null.
 * - `domainId`: required. The registry domain of the GIDs, for example 1 for Indonesia.
 * - `zoom` and `aggregation`: set both or neither. With both, rows roll up to the grid level
 *   for the zoom (see `levelForZoom`), capped at the finest level in the rows. `aggregation`
 *   is a rollup rule, for example `{ rule: "sum" }` or `{ rule: "weighted_mean", weight: "area" }`.
 *   Without zoom, every row keeps its own cell.
 * - `maxSegmentLengthM`: the core densifies ring edges up to this length in metres.
 *
 * shortcut: no cache across calls, so a big dataset rebuilds every ring on each render.
 * Upgrade with a cache keyed by GID when a map redraws often.
 */
export function cellPolygons(rows, options) {
  const { domainId, getGid, getValue, zoom, aggregation, maxSegmentLengthM } = options;
  if (!Number.isInteger(domainId)) throw new Error("domainId must be a whole number, the registry domain of the GIDs");
  if ((zoom == null) !== (aggregation == null)) throw new Error("zoom and aggregation must be set together");

  let items = rows.map((row) => ({ ...row, gid: getGid(row), value: getValue(row) }));
  if (zoom != null) {
    const finest = Math.min(...items.map((item) => decode(item.gid).level));
    const level = Math.min(levelForZoom(zoom), finest);
    items = rollupToLevel(items, level, { value: aggregation });
  }
  return items.map(({ gid, value }) => ({
    gid,
    value,
    polygon: cellPolygonWgs84(domainId, gid, maxSegmentLengthM, false),
  }));
}
