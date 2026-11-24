/**
 * CLI: total dan roll-up 1 km dari tabel people v2 di S3 untuk sebuah polygon (GeoJSON file atau WKT).
 *
 *   GEOSQUARE_EXTENSION=/opt/geosquare_v2.duckdb_extension AWS_PROFILE=default \
 *   npx tsx examples/node-query.ts area.geojson people_counts people_gen_z_night
 *   npx tsx examples/node-query.ts 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))'
 */
import { existsSync, readFileSync } from 'node:fs';
import { type Area, GeosquareCells } from '../src/node';

const PEOPLE = process.env.GEOSQUARE_DATASET ?? 's3://geosquare-data/apps-data/data_v2/data_name=people_v2023';
const [areaArg, ...cols] = process.argv.slice(2);
if (!areaArg) {
  console.error('usage: node-query.ts <area.geojson | WKT | xmin,ymin,xmax,ymax> [column ...]');
  process.exit(2);
}
const columns = cols.length ? cols : ['people_counts'];

function parseArea(a: string): Area {
  if (existsSync(a)) {
    const g = JSON.parse(readFileSync(a, 'utf8'));
    const geometry = g.type === 'FeatureCollection' ? g.features[0].geometry : g.type === 'Feature' ? g.geometry : g;
    return { geojson: geometry };
  }
  if (/^-?[\d.]+(,-?[\d.]+){3}$/.test(a)) return { bbox: a.split(',').map(Number) as [number, number, number, number] };
  return { wkt: a };
}

const gs = await GeosquareCells.open({
  extension: process.env.GEOSQUARE_EXTENSION ?? '../../build/linux_amd64/geosquare_v2.duckdb_extension',
  s3: { provider: 'credential_chain', region: process.env.AWS_REGION ?? 'ap-southeast-3' },
});
try {
  const area = parseArea(areaArg);
  let t = Date.now();
  const total = await gs.summarize(PEOPLE, area, columns);
  console.log(`total (${Date.now() - t} ms):`, total);
  t = Date.now();
  const km = await gs.rollup(PEOPLE, area, columns, 9);
  console.log(`1 km cells (${Date.now() - t} ms): ${km.length}`);
  console.table(km.slice(0, 10).map(({ geometry, ...r }) => r));
} finally {
  gs.close();
}
