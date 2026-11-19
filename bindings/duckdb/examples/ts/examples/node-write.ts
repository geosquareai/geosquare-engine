/**
 * CLI: tulis Parquet/CSV berkolom packed_id (atau gid / lon,lat) + nilai sebagai dataset sel v2.
 *
 *   npx tsx examples/node-write.ts input.parquet /data/my_dataset iceberg
 *   npx tsx examples/node-write.ts points.csv s3://bucket/prefix/my_dataset iceberg   # kolom lon, lat, value
 *
 * Titik (lon, lat) dijumlah per sel 50 m; NULL ditulis 0; partisi 10 km (level 7).
 */
import { GeosquareCells, sqlString } from '../src/node';

const [input, target, layout = 'iceberg'] = process.argv.slice(2);
if (!input || !target) {
  console.error('usage: node-write.ts <input.parquet|.csv> <target path> [flat|partitioned|iceberg]');
  process.exit(2);
}
const gs = await GeosquareCells.open({
  extension: process.env.GEOSQUARE_EXTENSION ?? '../../build/linux_amd64/geosquare_v2.duckdb_extension',
  s3: target.startsWith('s3://') ? { provider: 'credential_chain', region: process.env.AWS_REGION ?? 'ap-southeast-3' } : undefined,
});
try {
  const reader = input.endsWith('.csv') ? `read_csv_auto(${sqlString(input)})` : `read_parquet(${sqlString(input)})`;
  const cols = Object.keys(Object.fromEntries((await gs.query(`DESCRIBE SELECT * FROM ${reader}`)).map((r) => [String(r.column_name), 1])));
  const values = cols.filter((c) => !['packed_id', 'gid', 'lon', 'lat', 'longitude', 'latitude', 'geometry'].includes(c));
  let query: string;
  if (cols.includes('packed_id') || cols.includes('gid')) {
    query = `SELECT * FROM ${reader}`;
  } else {
    const lon = cols.includes('lon') ? 'lon' : 'longitude';
    const lat = cols.includes('lat') ? 'lat' : 'latitude';
    const sums = values.map((v) => `sum("${v}") AS "${v}"`).join(', ');
    query = `SELECT point_to_cell('ID', ${lon}, ${lat}, 12).packed_id AS packed_id${sums ? ', ' + sums : ', count(*) AS points'} FROM ${reader} GROUP BY 1`;
  }
  const t = Date.now();
  const r = await gs.write(query, target, {
    layout: layout as 'flat' | 'partitioned' | 'iceberg',
    mode: 'overwrite',
    partitionLevel: layout === 'flat' ? undefined : 7,
    valueSemantics: 'COUNT',
    aggregationRule: 'sum',
    columnMetrics: values.length > 100 ? 'key' : 'auto',
  });
  console.log(`written in ${Date.now() - t} ms:`, r);
  console.log('read back:', (await gs.query(`SELECT count(*) AS cells FROM read_cell_dataset(${sqlString(target)})`))[0]);
} finally {
  gs.close();
}
