import fs from 'node:fs';
import zlib from 'node:zlib';
import { streamNotes } from '../src/corpus.mjs';
const M = (n) => +(n / 1048576).toFixed(2);
const img = new Uint8Array(fs.readFileSync(process.argv[2] || 'contentless-8192.db'));
console.log('image MiB', M(img.byteLength));

for (const bs of [8192, 16384, 32768, 65536, 262144]) {
  let tot = 0;
  const t = performance.now();
  for (let o = 0; o < img.byteLength; o += bs)
    tot += zlib.deflateRawSync(img.subarray(o, Math.min(o + bs, img.byteLength)), { level: 6 }).byteLength;
  console.log('block=' + bs + ': ' + M(tot) + ' MiB ratio=' + (tot / img.byteLength).toFixed(3) + ' compress_ms=' + Math.round(performance.now() - t));
}
{
  const t = performance.now();
  const one = zlib.deflateRawSync(img, { level: 6 }).byteLength;
  console.log('whole-image single stream: ' + M(one) + ' MiB ratio=' + (one / img.byteLength).toFixed(3) + ' ms=' + Math.round(performance.now() - t));
}
if (zlib.zstdCompressSync) {
  const t = performance.now();
  let tot = 0;
  for (let o = 0; o < img.byteLength; o += 65536) tot += zlib.zstdCompressSync(img.subarray(o, Math.min(o + 65536, img.byteLength))).byteLength;
  console.log('zstd block=65536: ' + M(tot) + ' MiB ratio=' + (tot / img.byteLength).toFixed(3) + ' ms=' + Math.round(performance.now() - t));
}

const chunks = [];
let raw = 0;
for (const note of streamNotes()) for (const c of note.chunks) { chunks.push(c.content); raw += Buffer.byteLength(c.content, 'utf8'); }
console.log('chunks=' + chunks.length + ' raw content MiB=' + M(raw) + ' mean=' + Math.round(raw / chunks.length) + 'B');

const plain = chunks.reduce((a, s) => a + zlib.deflateRawSync(Buffer.from(s, 'utf8'), { level: 6 }).byteLength, 0);
console.log('per-chunk deflate: ' + M(plain) + ' MiB ratio=' + (plain / raw).toFixed(3));

const sample = chunks.filter((_, i) => i % 97 === 0).join(' ');
const dict = Buffer.from(sample, 'utf8').subarray(-32768);
const withDict = chunks.reduce((a, s) => a + zlib.deflateRawSync(Buffer.from(s, 'utf8'), { level: 9, dictionary: dict }).byteLength, 0);
console.log('per-chunk deflate+32KiB shared dict: ' + M(withDict) + ' MiB ratio=' + (withDict / raw).toFixed(3));

if (zlib.zstdCompressSync) {
  const zplain = chunks.reduce((a, s) => a + zlib.zstdCompressSync(Buffer.from(s, 'utf8')).byteLength, 0);
  console.log('per-chunk zstd(no dict): ' + M(zplain) + ' MiB ratio=' + (zplain / raw).toFixed(3));
}

for (const g of [8, 32, 128]) {
  let tot = 0;
  for (let i = 0; i < chunks.length; i += g) tot += zlib.deflateRawSync(Buffer.from(chunks.slice(i, i + g).join(' '), 'utf8'), { level: 6 }).byteLength;
  console.log('grouped deflate g=' + g + ': ' + M(tot) + ' MiB ratio=' + (tot / raw).toFixed(3));
}
