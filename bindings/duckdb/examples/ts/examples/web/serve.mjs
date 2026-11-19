// Build the demo and serve it: node examples/web/serve.mjs [dataset dir] [extension .wasm]
// Then open http://127.0.0.1:8798/www/ (DuckDB-Wasm needs http, not file://).
import { build } from 'esbuild';
import { createServer } from 'node:http';
import { cpSync, existsSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import { extname, join, resolve } from 'node:path';

const [data, ext = '../../build/wasm_eh/geosquare_v2.duckdb_extension.wasm'] = process.argv.slice(2);
mkdirSync('www', { recursive: true });
cpSync('examples/web/index.html', 'www/index.html');
cpSync(resolve(ext), 'www/geosquare_v2.duckdb_extension.wasm');
if (data) cpSync(data, 'www/data', { recursive: true });
await build({ entryPoints: ['examples/web/main.ts'], bundle: true, format: 'esm', outfile: 'www/demo.js', platform: 'browser' });
const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json' };
createServer((req, res) => {
  const path = join(process.cwd(), decodeURIComponent(new URL(req.url, 'http://x').pathname));
  const file = existsSync(path) && statSync(path).isDirectory() ? join(path, 'index.html') : path;
  if (!existsSync(file)) return res.writeHead(404).end();
  const body = readFileSync(file);
  const range = req.headers.range?.match(/bytes=(\d+)-(\d*)/);
  const head = { 'Content-Type': types[extname(file)] ?? 'application/octet-stream', 'Access-Control-Allow-Origin': '*', 'Accept-Ranges': 'bytes' };
  if (range) {
    const a = Number(range[1]), b = range[2] ? Number(range[2]) : body.length - 1;
    res.writeHead(206, { ...head, 'Content-Range': `bytes ${a}-${b}/${body.length}`, 'Content-Length': b - a + 1 }).end(body.subarray(a, b + 1));
  } else res.writeHead(200, { ...head, 'Content-Length': body.length }).end(req.method === 'HEAD' ? undefined : body);
}).listen(8798, '127.0.0.1', () => console.log('open http://127.0.0.1:8798/www/'));
