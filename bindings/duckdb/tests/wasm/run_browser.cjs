// node run_browser.cjs <base url> <expected.json> [chromium executable]
const { chromium } = require("playwright-core");
const fs = require("fs");
(async () => {
  const [base, expectedPath, exe] = process.argv.slice(2);
  const expected = JSON.parse(fs.readFileSync(expectedPath, "utf8"));
  const browser = await chromium.launch({ executablePath: exe || undefined, args: ["--no-sandbox"] });
  const page = await browser.newPage();
  page.on("pageerror", (e) => console.log("pageerror:", e.message));
  await page.goto(`${base}/test.html`);
  await page.waitForFunction(() => window.__results, null, { timeout: 180000 });
  const got = await page.evaluate(() => window.__results);
  console.log(`chromium ${await browser.version()}, DuckDB-Wasm ${got.duckdb}`);
  await browser.close();
  if (got.error) throw new Error(got.error);
  let ok = true;
  for (const [k, v] of Object.entries(expected)) {
    const same = JSON.stringify(got[k]) === JSON.stringify(v);
    ok = ok && same;
    console.log(`${same ? "ok  " : "FAIL"} ${k.padEnd(18)} wasm=${JSON.stringify(got[k])} native=${JSON.stringify(v)}`);
  }
  process.exit(ok ? 0 : 1);
})().catch((e) => { console.error("FAIL", e.message); process.exit(1); });
