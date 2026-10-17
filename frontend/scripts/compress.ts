// Writes a `.br` and a `.gz` beside each file of the client build that is worth compressing,
// so the control-server can send it compressed (`ServeDir::precompressed_br` and
// `precompressed_gzip` in `web.rs`). Run after `react-router build`.
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

const DIR = "build/client";
/** Text files only: images and fonts are compressed already. */
const TYPES = /\.(js|css|html|svg|json|txt|map)$/;
/** Smaller files gain too little to be worth a second request path. */
const MIN_BYTES = 1024;

function* files(dir: string): Generator<string> {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else if (TYPES.test(name)) yield path;
  }
}

let before = 0;
let after = 0;
for (const path of files(DIR)) {
  const data = readFileSync(path);
  if (data.length < MIN_BYTES) continue;
  const br = brotliCompressSync(data, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } });
  writeFileSync(`${path}.br`, br);
  writeFileSync(`${path}.gz`, gzipSync(data, { level: 9 }));
  before += data.length;
  after += br.length;
}
console.log(`compressed ${(before / 1e6).toFixed(2)} MB of text to ${(after / 1e6).toFixed(2)} MB (brotli)`);
