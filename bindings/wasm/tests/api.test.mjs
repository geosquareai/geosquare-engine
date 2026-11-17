// Checks the JavaScript API: value types, the BigInt rule for packed IDs, and loud failures
// for wrong input. The vector tests cover the values; these tests cover the boundary.
import { test } from "node:test";
import assert from "node:assert/strict";
import { gs } from "./load.mjs";

test("version is a string", () => {
  assert.equal(typeof gs.version(), "string");
});

test("return types are JavaScript primitives or plain objects", () => {
  assert.equal(typeof gs.encode(3, 1, 2), "string");
  const cell = gs.decode(gs.encode(4, 15, 1));
  assert.deepEqual([cell.level, cell.xIdx, cell.yIdx], [4, 15, 1]);
  assert.equal(typeof gs.pack(1, "2345"), "bigint");
  const id = gs.unpack(gs.pack(7, "2345"));
  assert.deepEqual([id.domainId, id.gid], [7, "2345"]);
});

test("packed IDs are BigInt only, never Number", () => {
  assert.throws(() => gs.unpack(5), /must be a BigInt/);
  assert.throws(() => gs.unpack(5.0), /must be a BigInt/);
});

test("a BigInt outside Int64 throws instead of wrapping", () => {
  const packed = gs.pack(1, "2345");
  // Without the range check, 2^64 + packed would wrap to the valid value `packed`.
  assert.throws(() => gs.unpack(2n ** 64n + packed), /outside the Int64 range/);
  assert.throws(() => gs.unpack(2n ** 63n), /outside the Int64 range/);
});

test("numeric arguments must be whole and in range", () => {
  assert.throws(() => gs.encode(256, 0, 0), /out of range/);
  assert.throws(() => gs.encode(2, 1.5, 0), /whole number/);
  assert.throws(() => gs.encode(null, 0, 0), /must be a number/);
  assert.throws(() => gs.encode("3", 0, 0), /must be a number/);
  assert.throws(() => gs.encode(2, -1, 0), /whole number/);
  assert.throws(() => gs.encode(2, Number.NaN, 0), /whole number/);
});

test("level above 14 and domain 0 throw with the core message", () => {
  assert.throws(() => gs.encode(15, 0, 0), /outside 0..=14/);
  assert.throws(() => gs.pack(0, "2345"), /domain ID 0/);
});

test("root parent is undefined and level 14 has no children", () => {
  assert.equal(gs.parent(""), undefined);
  assert.throws(() => gs.children("2".repeat(14)), /outside 0..=14/);
});

test("out-of-range indices throw", () => {
  assert.throws(() => gs.encode(1, 5, 0), /outside the level 1 grid/);
});

test("distance between levels names the mismatch", () => {
  assert.throws(() => gs.distanceM("2345", "23456"), /different levels/);
});

test("child level above parent is named", () => {
  assert.throws(() => gs.descendantRange(1, "23456", 3), /above parent level/);
});

test("kDisk with a huge k clips to the grid", () => {
  const gid = gs.encode(2, 3, 4);
  assert.equal(gs.kDisk(gid, Number.MAX_SAFE_INTEGER).length, 100);
  assert.deepEqual(gs.kRing(gid, Number.MAX_SAFE_INTEGER), []);
});

test("detectTable rejects booleans, NaN and fractions", () => {
  assert.throws(() => gs.detectTable({ flag: ["int64", [true]] }), /must be a string, BigInt/);
  assert.throws(() => gs.detectTable({ gid: ["string", ["23456", Number.NaN]] }), /whole/);
  assert.throws(() => gs.detectTable({ gid: ["string", ["23456", 0.5]] }), /whole/);
});

test("detectTable refuses Numbers above 2^53 and BigInts above Int64", () => {
  assert.throws(() => gs.detectTable({ v: ["int64", [2 ** 53]] }), /pass a BigInt/);
  assert.throws(() => gs.detectTable({ v: ["int64", [2n ** 63n]] }), /outside the Int64 range/);
});

test("detectTable rejects an unknown dtype and a bad level", () => {
  assert.throws(() => gs.detectTable({ x: ["float64", [1.5]] }), /dtype/);
  assert.throws(() => gs.detectTable({ x: ["string", []] }, null, 256), /out of range/);
});

test("detectTable accepts a float-like column when its dtype is other", () => {
  const [result] = gs.detectTable({ ratio: ["other", [0.5, null]] });
  assert.equal(result.kind, "none");
  assert.equal(result.status, "rejected");
});

test("bare GIDs need a domain, and the level list is filled in", () => {
  const [result] = gs.detectTable({ gid: ["string", ["23456", "H2X54"]] });
  assert.equal(result.kind, "gid");
  assert.equal(result.status, "needs_domain");
  assert.deepEqual([...result.levels], [5]);
});

test("x_idx and y_idx need a level, and are certain with one", () => {
  const columns = { x_idx: ["int64", [1n, 2n]], y_idx: ["int64", [3n, 4n]] };
  const [pending] = gs.detectTable(columns, "ID");
  assert.equal(pending.column, "x_idx+y_idx");
  assert.equal(pending.status, "needs_level");
  const [certain] = gs.detectTable(columns, "ID", 2);
  assert.equal(certain.kind, "xy_indices");
  assert.equal(certain.status, "certain");
  assert.deepEqual([...certain.domainCodes], ["ID"]);
  assert.deepEqual([...certain.levels], [2]);
});

test("detection result keys are stable", () => {
  const [result] = gs.detectTable({ gid: ["string", ["23456"]] }, "ID");
  assert.deepEqual(Object.keys(result).sort(), [
    "column",
    "domainCodes",
    "domainIds",
    "invalidExamples",
    "kind",
    "levels",
    "reason",
    "status",
    "validRatio",
  ]);
});
