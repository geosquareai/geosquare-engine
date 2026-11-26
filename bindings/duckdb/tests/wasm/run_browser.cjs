// node run_browser.cjs <page url> <expected.json> [chromium executable]
const { chromium } = require("playwright-core");
const fs = require("fs");
(async () => {
  const [url, expectedPath, exe] = process.argv.slice(2);
  const expected = JSON.parse(fs.readFileSync(expectedPath, "utf8"));
  const browser = await chromium.launch({ executablePath: exe || undefined, args: ["--no-sandbox"] });
  const page = await browser.newPage();
  page.on("pageerror", (e) => console.log("pageerror:", e.message));
  await page.goto(url);
  try {
    await page.waitForFunction(() => window.__results, null, { timeout: 180000 });
  } catch (e) {
    const steps = await page.evaluate(() => window.__progress || []).catch(() => []);
    await browser.close();
    throw new Error(`${e.message} (stalled at: ${steps.slice(-1)[0] || "page load"})`);
  }
  const got = await page.evaluate(() => window.__results);
  console.log(`chromium ${await browser.version()}, DuckDB-Wasm ${got.duckdb} (${got.platform})`);
  await browser.close();
  if (got.error) {
    const done = Object.keys(got).filter((k) => !["error", "duckdb", "platform"].includes(k));
    throw new Error(`${got.error} (passed before the error: ${done.join(", ") || "nothing"})`);
  }
  let ok = true;
  let checks = Object.entries(expected);
  if (got.parquet_unavailable) {
    console.log(`skip dataset checks: DuckDB's parquet extension does not load in this bundle (${got.parquet_unavailable})`);
    checks = checks.filter(([k]) => ["point_to_cell", "v1_gid", "cells", "period_labels"].includes(k));
  }
  for (const [k, v] of checks) {
    const same = JSON.stringify(got[k]) === JSON.stringify(v);
    ok = ok && same;
    console.log(`${same ? "ok  " : "FAIL"} ${k.padEnd(18)} wasm=${JSON.stringify(got[k])} native=${JSON.stringify(v)}`);
  }
  process.exit(ok ? 0 : 1);
})().catch((e) => { console.error("FAIL", e.message); process.exit(1); });
