// Runs the spec 2.0 conformance vectors against the WebAssembly binding.
// The expected values come from the same files the Rust and Python tests read.
import { test } from "node:test";
import assert from "node:assert/strict";
import { gs, vector } from "./load.mjs";

const domainIds = () => vector("codec.json").packed_ids_by_domain.domains;

function descendantGids(parent, childLevel) {
  let gids = [parent];
  for (let level = parent.length; level < childLevel; level += 1) {
    gids = gids.flatMap((gid) => [...gs.children(gid)]);
  }
  return gids;
}

const sorted = (values) => [...values].sort();

test("vector set is complete", () => {
  assert.equal(Object.keys(domainIds()).length, 11);
  assert.ok(vector("detect.json").cases.length >= 15);
});

test("codec cells round trip and pack for ID", () => {
  for (const record of vector("codec.json").cells) {
    assert.equal(gs.encode(record.level, record.x_idx, record.y_idx), record.gid);
    const cell = gs.decode(record.gid);
    assert.deepEqual([cell.level, cell.xIdx, cell.yIdx], [record.level, record.x_idx, record.y_idx]);
    const packed = BigInt(record.packed_id);
    assert.equal(gs.pack(1, record.gid), packed, record.gid);
    const id = gs.unpack(packed);
    assert.deepEqual([id.domainId, id.gid], [1, record.gid]);
  }
});

test("packed IDs match for every domain", () => {
  const codec = vector("codec.json").packed_ids_by_domain;
  for (const record of codec.cells) {
    assert.equal(gs.encode(record.level, record.x_idx, record.y_idx), record.gid);
    for (const [code, domainId] of Object.entries(codec.domains)) {
      const expected = BigInt(record.packed_ids[code]);
      assert.equal(gs.pack(domainId, record.gid), expected, `domain ${code}`);
      const id = gs.unpack(expected);
      assert.deepEqual([id.domainId, id.gid], [domainId, record.gid], `domain ${code}`);
    }
  }
});

test("invalid GIDs throw", () => {
  for (const record of vector("codec.json").invalid_gids) {
    assert.throws(() => gs.decode(record.input), Error, record.input);
  }
});

test("invalid packed IDs throw", () => {
  for (const record of vector("codec.json").invalid_packed_ids) {
    assert.throws(() => gs.unpack(BigInt(record.packed_id)), Error, record.packed_id);
  }
});

test("parents and children match", () => {
  const topology = vector("topology.json");
  for (const record of topology.parent_of) {
    const parent = gs.parent(record.gid);
    assert.equal(parent, record.parent_gid);
    assert.equal(parent.length, record.level - 1);
  }
  for (const record of topology.children_of) {
    assert.deepEqual(sorted(gs.children(record.gid)), sorted(record.child_gids), record.gid);
  }
});

test("descendant ranges match and contain every descendant", () => {
  for (const record of vector("topology.json").descendant_ranges) {
    const range = gs.descendantRange(1, record.parent_gid, record.child_level);
    assert.equal(range.lo, BigInt(record.lo));
    assert.equal(range.hi, BigInt(record.hi));
    const descendants = descendantGids(record.parent_gid, record.child_level);
    assert.equal(descendants.length, record.descendant_count);
    for (const gid of descendants) {
      const packed = gs.pack(1, gid);
      assert.ok(range.lo <= packed && packed <= range.hi, gid);
    }
  }
});

test("descendant ranges match for every domain", () => {
  const domains = domainIds();
  const records = vector("topology.json").descendant_ranges_by_domain.records;
  assert.ok(records.length > 0);
  for (const record of records) {
    for (const [code, bounds] of Object.entries(record.ranges)) {
      const range = gs.descendantRange(domains[code], record.parent_gid, record.child_level);
      assert.equal(range.lo, BigInt(bounds.lo), code);
      assert.equal(range.hi, BigInt(bounds.hi), code);
    }
  }
});

test("neighbour sets match", () => {
  for (const record of vector("topology.json").neighbours) {
    const gid = gs.encode(record.level, record.x_idx, record.y_idx);
    assert.deepEqual(sorted(gs.kRing(gid, 1)), sorted(record.k_ring_1), gid);
    assert.deepEqual(sorted(gs.kRing(gid, 2)), sorted(record.k_ring_2), gid);
    assert.deepEqual(sorted(gs.kDisk(gid, 1)), sorted(record.k_disk_1), gid);
  }
});

test("distances match the square root formula", () => {
  for (const record of vector("topology.json").distances) {
    assert.equal(gs.cellSideM(record.a_gid.length), record.cell_side_m);
    const expected = record.distance_m;
    const actual = gs.distanceM(record.a_gid, record.b_gid);
    const tolerance = 1e-12 * Math.max(Math.abs(expected), 1);
    assert.ok(Math.abs(actual - expected) <= tolerance, `${actual} != ${expected}`);
  }
});

const detectDtype = (dtype) => (dtype === "string" || dtype === "int64" ? dtype : "other");

// Int64 values are stored as decimal strings in the vector. They become BigInt here.
const detectValue = (dtype, value) =>
  dtype === "int64" && value !== null ? BigInt(value) : value;

test("detection cases match the reference", () => {
  for (const testCase of vector("detect.json").cases) {
    const table = {};
    for (const [name, spec] of Object.entries(testCase.columns)) {
      const dtype = detectDtype(spec.dtype);
      table[name] = [dtype, spec.values.map((value) => detectValue(dtype, value))];
    }
    const results = gs.detectTable(table, testCase.domain_code ?? null, testCase.level ?? null);
    const byColumn = Object.fromEntries(results.map((result) => [result.column, result]));
    for (const [key, expected] of Object.entries(testCase.expect)) {
      const result = byColumn[key];
      const label = `${testCase.name}/${key}`;
      assert.ok(result, label);
      assert.equal(result.kind, expected.kind, label);
      assert.equal(result.status, expected.status, label);
      if ("domain_codes" in expected) {
        assert.deepEqual([...result.domainCodes], expected.domain_codes, label);
      }
      if ("domain_ids" in expected) {
        assert.deepEqual([...result.domainIds], expected.domain_ids, label);
      }
      if ("levels" in expected) {
        assert.deepEqual([...result.levels], expected.levels, label);
      }
    }
  }
});
