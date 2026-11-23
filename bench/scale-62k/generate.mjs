#!/usr/bin/env node
// Deterministic synthetic Markdown vault for scale measurements.
//
//   node bench/scale-62k/generate.mjs --notes 62000 --seed 62000 --out <dir>
//
// Writes <dir>/vault/ (the notes), <dir>/manifest.tsv (path, bytes, sha256,
// sorted by path bytes) and <dir>/README.md (seed, generator commit, totals).
// The output directory must not exist yet. Everything is generated from a
// seeded PRNG: invented vocabulary, invented names, no real content. Re-running
// with the same --notes/--seed on the same generator commit produces
// byte-identical files and an identical manifest.
//
// Shape (same distributions at every --notes):
// - nested folder tree, depth 1-5, roughly 40 notes per folder;
// - log-normal note size (median ~2 KiB) with a long tail capped near 50 KiB,
//   plus a slice of tiny stub notes;
// - YAML frontmatter on ~45% of notes (title, tags, aliases, status, date);
// - headings, paragraphs, bullet and numbered lists, task lists, fenced code,
//   occasional tables and block quotes;
// - wikilinks to other generated notes ([[name]], [[name|alias]],
//   [[folder/name]], [[name#Heading]]) plus a few dangling links;
// - repeated basenames (index, todo, meeting notes, ...) in many folders;
// - non-ASCII words, names, headings, file and folder names (Latin
//   diacritics, Greek, Cyrillic, CJK, emoji) on a minority of notes.

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const GENERATOR_VERSION = 1;
const SIZE_CAP = 50 * 1024;
const NOTES_PER_FOLDER = 40;

function parseArgs(argv) {
  const args = { notes: 62000, seed: 62000, out: null };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (flag === '--notes') args.notes = Number.parseInt(value, 10);
    else if (flag === '--seed') args.seed = Number.parseInt(value, 10);
    else if (flag === '--out') args.out = value;
    else throw new Error(`unknown argument ${flag}`);
    i += 1;
  }
  if (!args.out) throw new Error('--out <dir> is required');
  if (!Number.isInteger(args.notes) || args.notes < 10) throw new Error('--notes must be >= 10');
  if (!Number.isInteger(args.seed)) throw new Error('--seed must be an integer');
  return args;
}

function mulberry32(seed) {
  let state = seed >>> 0;
  return function next() {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function mix(a, b) {
  let h = Math.imul(a ^ 0x9e3779b9, 0x85ebca6b) ^ Math.imul(b + 0x632be5ab, 0xc2b2ae35);
  h ^= h >>> 16;
  h = Math.imul(h, 0x7feb352d);
  h ^= h >>> 15;
  return h >>> 0;
}

class Rng {
  constructor(seed) {
    this.next = mulberry32(seed);
  }
  float() {
    return this.next();
  }
  int(lo, hi) {
    return lo + Math.floor(this.next() * (hi - lo + 1));
  }
  chance(p) {
    return this.next() < p;
  }
  pick(list) {
    return list[Math.floor(this.next() * list.length)];
  }
  normal() {
    const u = Math.max(this.next(), 1e-12);
    const v = this.next();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }
}

// Invented vocabulary: pronounceable syllable words, Zipf-distributed so that
// some words are everywhere and most are rare, like real prose.
const ONSETS = ['b', 'br', 'c', 'ch', 'd', 'dr', 'f', 'g', 'gl', 'h', 'k', 'l', 'm', 'n', 'p', 'pl', 'qu', 'r', 's', 'sh', 'st', 't', 'th', 'tr', 'v', 'w', 'z'];
const NUCLEI = ['a', 'e', 'i', 'o', 'u', 'ai', 'ea', 'io', 'ou', 'y'];
const CODAS = ['', '', 'n', 'r', 'l', 's', 'th', 'nd', 'rk', 'm', 'x', 'st'];
const FUNCTION_WORDS = ['the', 'a', 'of', 'and', 'to', 'in', 'is', 'for', 'on', 'with', 'as', 'by', 'at', 'from', 'that', 'this', 'it', 'or', 'be', 'are', 'was', 'not', 'but', 'we', 'when', 'then', 'into', 'over', 'after', 'before'];
const NON_ASCII_WORDS = [
  'café', 'naïve', 'façade', 'smörgåsbord', 'jalapeño', 'über', 'Ångström', 'crème', 'São', 'Zürich',
  'Ælfric', 'déjà', 'piñata', 'fjäll', 'œuvre', 'Łódź', 'Dvořák', 'Ísafjörður', 'résumé', 'Straße',
  'λόγος', 'θάλασσα', 'κῆπος', 'дерево', 'облако', 'сад', 'река', '庭園', '月光', '図書館', '山脈',
  '정원', '바다', 'حديقة', 'בית', 'नदी', 'ทะเล',
];
const EMOJI = ['🌿', '📚', '🔭', '🧭', '🌙', '⚙️', '🗂️'];
const NAME_FIRST = ['Avelin', 'Brisk', 'Corvane', 'Delphi', 'Esker', 'Fennick', 'Galeth', 'Hollis', 'Ilvra', 'Jorvan', 'Kestrel', 'Lumen', 'Maelis', 'Norrin', 'Orsolya', 'Perrin', 'Quillon', 'Rhoswen', 'Saffi', 'Tamsin', 'Ulric', 'Vesna', 'Wren', 'Ysolde', 'Zephan', 'Ånja', 'Émeric', 'Søren', 'Željko', 'Íde'];
const NAME_LAST = ['Ashgrove', 'Brindlemoor', 'Coldwater', 'Dunmere', 'Elmstead', 'Fallowfield', 'Greystone', 'Hawkridge', 'Ironvale', 'Juniper', 'Kettleby', 'Larkspur', 'Mossbank', 'Northwick', 'Oakhaven', 'Pellbrook', 'Quarrymoor', 'Ravensholt', 'Stillwater', 'Thornbury', 'Underhill', 'Valewood', 'Whitlock', 'Yarrowby', 'Østergaard', 'Müllerhof', 'Novák'];
const LANGS = ['python', 'rust', 'ts', 'bash', 'json', 'yaml', 'sql', 'toml', ''];
const STATUSES = ['draft', 'active', 'review', 'done', 'archived', 'idea'];
const DUP_BASENAMES = ['index', 'todo', 'meeting notes', 'README', 'scratch', 'overview', 'ideas', 'log', 'inbox', 'questions', 'notes', 'summary'];
const TOP_FOLDERS = ['Projects', 'Areas', 'Resources', 'Archive', 'Journal', 'People', 'Meetings', 'Research', 'Reference', 'Inbox', 'Templates', 'Atlas', 'Clients', 'Learning', 'Writing', 'Ops'];
const VOCAB_SIZE = 40000;
const ZIPF_S = 1.05;

function makeVocabulary(rng) {
  const seen = new Set(FUNCTION_WORDS);
  const words = [];
  while (words.length < VOCAB_SIZE) {
    // Shorter words for frequent ranks, longer for rare ones.
    const rankBand = words.length < 300 ? 1 : words.length < 5000 ? 2 : 3;
    const syllables = rankBand === 1 ? rng.int(1, 2) : rankBand === 2 ? rng.int(2, 3) : rng.int(2, 4);
    let word = '';
    for (let s = 0; s < syllables; s += 1) {
      word += rng.pick(ONSETS) + rng.pick(NUCLEI) + (s === syllables - 1 ? rng.pick(CODAS) : '');
    }
    if (word.length < 3 || seen.has(word)) continue;
    seen.add(word);
    words.push(word);
  }
  return words;
}

function makeZipf(words) {
  const cumulative = new Float64Array(words.length);
  let total = 0;
  for (let r = 0; r < words.length; r += 1) {
    total += 1 / Math.pow(r + 1, ZIPF_S);
    cumulative[r] = total;
  }
  return (rng) => {
    const target = rng.float() * total;
    let lo = 0;
    let hi = words.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (cumulative[mid] < target) lo = mid + 1;
      else hi = mid;
    }
    return words[lo];
  };
}

function capitalize(word) {
  return word.charAt(0).toUpperCase() + word.slice(1);
}

function safeSegment(text) {
  // Keep names portable: no path separators, reserved characters or
  // leading/trailing dots and spaces.
  return text.replace(/[\\/:*?"<>|#^[\]]/g, '').replace(/^[.\s]+|[.\s]+$/g, '') || 'untitled';
}

function buildFolders(rng, noteCount, sample) {
  const folderCount = Math.max(4, Math.round(noteCount / NOTES_PER_FOLDER));
  const folders = [];
  const used = new Set();
  const tops = TOP_FOLDERS.slice(0, Math.min(TOP_FOLDERS.length, Math.max(4, Math.round(Math.sqrt(folderCount)))));
  for (const top of tops) {
    folders.push(top);
    used.add(top);
  }
  let guard = 0;
  while (folders.length < folderCount && guard < folderCount * 50) {
    guard += 1;
    const parent = rng.pick(folders);
    if (parent.split('/').length >= 5) continue;
    let name;
    const roll = rng.float();
    if (roll < 0.05) name = rng.pick(NON_ASCII_WORDS);
    else if (roll < 0.15) name = `${2018 + rng.int(0, 8)}`;
    else if (roll < 0.25) name = `${rng.pick(NAME_FIRST)} ${rng.pick(NAME_LAST)}`;
    else name = capitalize(sample(rng)) + (rng.chance(0.4) ? ` ${sample(rng)}` : '');
    const path = `${parent}/${safeSegment(name)}`;
    if (used.has(path.toLowerCase())) continue;
    used.add(path.toLowerCase());
    folders.push(path);
  }
  return folders;
}

function buildNotes(rng, noteCount, folders, sample) {
  const notes = [];
  const usedPaths = new Set();
  for (let i = 0; i < noteCount; i += 1) {
    const folder = rng.pick(folders);
    let base;
    const roll = rng.float();
    if (roll < 0.06) base = rng.pick(DUP_BASENAMES);
    else if (roll < 0.10) base = `${2018 + rng.int(0, 8)}-${String(rng.int(1, 12)).padStart(2, '0')}-${String(rng.int(1, 28)).padStart(2, '0')}`;
    else if (roll < 0.14) base = `${rng.pick(NAME_FIRST)} ${rng.pick(NAME_LAST)}`;
    else if (roll < 0.18) base = `${capitalize(sample(rng))} ${rng.pick(NON_ASCII_WORDS)}`;
    else if (roll < 0.19) base = `${capitalize(sample(rng))} ${rng.pick(EMOJI)}`;
    else {
      const words = rng.int(1, 5);
      const parts = [];
      for (let w = 0; w < words; w += 1) parts.push(w === 0 ? capitalize(sample(rng)) : sample(rng));
      base = parts.join(' ');
    }
    base = safeSegment(base);
    let path = `${folder}/${base}.md`;
    let suffix = 2;
    while (usedPaths.has(path.toLowerCase())) {
      path = `${folder}/${base} ${suffix}.md`;
      suffix += 1;
    }
    usedPaths.add(path.toLowerCase());
    notes.push({ path, base: path.slice(folder.length + 1, -3), folder });
  }
  return notes;
}

function targetSize(rng) {
  if (rng.chance(0.07)) return rng.int(40, 400); // stubs
  // log-normal, median ~2 KiB, sigma ~1.1 gives a long right tail.
  const size = Math.round(Math.exp(Math.log(2048) + 1.1 * rng.normal()));
  return Math.min(SIZE_CAP, Math.max(120, size));
}

function sentence(rng, sample, links, nonAscii) {
  const length = rng.int(6, 22);
  const words = [];
  for (let w = 0; w < length; w += 1) {
    const roll = rng.float();
    if (roll < 0.32) words.push(rng.pick(FUNCTION_WORDS));
    else if (nonAscii && roll < 0.36) words.push(rng.pick(NON_ASCII_WORDS));
    else if (roll < 0.375) words.push(`${rng.pick(NAME_FIRST)} ${rng.pick(NAME_LAST)}`);
    else if (roll < 0.39) words.push(String(rng.int(1, 9999)));
    else if (roll < 0.40 && links.length > 0) words.push(rng.pick(links)());
    else if (roll < 0.405) words.push(`\`${sample(rng)}_${sample(rng)}\``);
    else if (roll < 0.41) words.push(`**${sample(rng)}**`);
    else if (roll < 0.415) words.push(`*${sample(rng)}*`);
    else if (roll < 0.418) words.push(`#${sample(rng)}`);
    else words.push(sample(rng));
  }
  return `${capitalize(words.join(' '))}${rng.pick(['.', '.', '.', '?', '!', ';'])}`;
}

function codeBlock(rng, sample) {
  const lang = rng.pick(LANGS);
  const lines = rng.int(2, 18);
  const body = [];
  for (let l = 0; l < lines; l += 1) {
    const indent = '  '.repeat(rng.int(0, 2));
    const a = sample(rng);
    const b = sample(rng);
    const shapes = [
      `${indent}${a}_${b} = ${rng.int(0, 999)}`,
      `${indent}fn ${a}(${b}: u32) -> u32 { ${b} + ${rng.int(1, 9)} }`,
      `${indent}"${a}": "${b}",`,
      `${indent}${a} --${b} ${rng.int(1, 64)}`,
      `${indent}SELECT ${a} FROM ${b} WHERE id = ${rng.int(1, 500)};`,
      `${indent}# ${a} ${b}`,
    ];
    body.push(rng.pick(shapes));
  }
  return `\`\`\`${lang}\n${body.join('\n')}\n\`\`\``;
}

function renderNote(index, notes, rng, sample) {
  const note = notes[index];
  const target = targetSize(rng);
  const nonAscii = rng.chance(0.12);
  const linkCount = rng.chance(0.75) ? rng.int(1, 8) : 0;
  const linkMakers = [];
  for (let l = 0; l < linkCount; l += 1) {
    const other = notes[Math.floor(rng.float() * notes.length)];
    const roll = rng.float();
    linkMakers.push(() => {
      if (roll < 0.05) return `[[${capitalize(sample(rng))} missing ${sample(rng)}]]`;
      if (roll < 0.25) return `[[${other.path.slice(0, -3)}]]`;
      if (roll < 0.45) return `[[${other.base}|${sample(rng)} ${sample(rng)}]]`;
      if (roll < 0.55) return `[[${other.base}#${capitalize(sample(rng))}]]`;
      return `[[${other.base}]]`;
    });
  }
  const parts = [];
  if (rng.chance(0.45)) {
    const fm = ['---'];
    if (rng.chance(0.7)) fm.push(`title: "${capitalize(note.base.replace(/"/g, ''))}"`);
    const tagCount = rng.int(0, 4);
    if (tagCount > 0) {
      fm.push('tags:');
      for (let t = 0; t < tagCount; t += 1) fm.push(`  - ${sample(rng)}${rng.chance(0.2) ? `/${sample(rng)}` : ''}`);
    }
    if (rng.chance(0.2)) fm.push(`aliases: [${capitalize(sample(rng))} ${sample(rng)}]`);
    if (rng.chance(0.5)) fm.push(`status: ${rng.pick(STATUSES)}`);
    if (rng.chance(0.5)) fm.push(`date: ${2018 + rng.int(0, 8)}-${String(rng.int(1, 12)).padStart(2, '0')}-${String(rng.int(1, 28)).padStart(2, '0')}`);
    if (rng.chance(0.15)) fm.push(`owner: ${rng.pick(NAME_FIRST)} ${rng.pick(NAME_LAST)}`);
    if (rng.chance(0.1)) fm.push(`priority: ${rng.int(1, 5)}`);
    fm.push('---');
    parts.push(fm.join('\n'));
  }
  if (rng.chance(0.8)) parts.push(`# ${capitalize(note.base)}`);
  let size = parts.reduce((n, p) => n + Buffer.byteLength(p, 'utf8') + 2, 0);
  let headingDepth = 2;
  while (size < target) {
    const roll = rng.float();
    let block;
    if (roll < 0.12) {
      headingDepth = rng.int(2, 4);
      const title = [capitalize(sample(rng)), sample(rng)];
      if (nonAscii && rng.chance(0.3)) title.push(rng.pick(NON_ASCII_WORDS));
      block = `${'#'.repeat(headingDepth)} ${title.join(' ')}`;
    } else if (roll < 0.22) {
      const items = rng.int(2, 7);
      const ordered = rng.chance(0.3);
      const task = !ordered && rng.chance(0.25);
      const lines = [];
      for (let i = 0; i < items; i += 1) {
        const prefix = ordered ? `${i + 1}.` : task ? `- [${rng.chance(0.4) ? 'x' : ' '}]` : '-';
        const nest = !ordered && rng.chance(0.15) ? '  ' : '';
        lines.push(`${nest}${prefix} ${sentence(rng, sample, linkMakers, nonAscii)}`);
      }
      block = lines.join('\n');
    } else if (roll < 0.27) {
      block = codeBlock(rng, sample);
    } else if (roll < 0.29) {
      const cols = rng.int(2, 4);
      const rows = rng.int(2, 6);
      const header = Array.from({ length: cols }, () => capitalize(sample(rng)));
      const lines = [`| ${header.join(' | ')} |`, `|${' --- |'.repeat(cols)}`];
      for (let r = 0; r < rows; r += 1) lines.push(`| ${Array.from({ length: cols }, () => sample(rng)).join(' | ')} |`);
      block = lines.join('\n');
    } else if (roll < 0.31) {
      block = `> ${sentence(rng, sample, linkMakers, nonAscii)}`;
    } else {
      const count = rng.int(1, 6);
      const sentences = [];
      for (let s = 0; s < count; s += 1) sentences.push(sentence(rng, sample, linkMakers, nonAscii));
      block = sentences.join(' ');
    }
    parts.push(block);
    size += Buffer.byteLength(block, 'utf8') + 2;
  }
  // Guarantee at least one link from linked notes so the graph is dense enough.
  if (linkMakers.length > 0 && rng.chance(0.5)) parts.push(`Related: ${linkMakers.map((m) => m()).slice(0, 4).join(', ')}`);
  let text = `${parts.join('\n\n')}\n`;
  if (Buffer.byteLength(text, 'utf8') > SIZE_CAP + 2048) {
    // Trim on a character boundary so oversize notes stay near the cap.
    text = `${Buffer.from(text, 'utf8').subarray(0, SIZE_CAP).toString('utf8').replace(/�+$/u, '')}\n`;
  }
  return text;
}

function generatorCommit() {
  const here = dirname(fileURLToPath(import.meta.url));
  try {
    const head = execFileSync('git', ['-C', here, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
    const dirty = execFileSync('git', ['-C', here, 'status', '--porcelain', '--', '.'], { encoding: 'utf8' }).trim();
    return dirty ? `${head} (generator directory modified)` : head;
  } catch {
    return 'unknown';
  }
}

function compareBytes(a, b) {
  return Buffer.compare(Buffer.from(a, 'utf8'), Buffer.from(b, 'utf8'));
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const out = resolve(args.out);
  if (existsSync(out)) throw new Error(`output directory already exists: ${out}`);
  const vault = join(out, 'vault');
  const structureRng = new Rng(mix(args.seed, 1));
  const words = makeVocabulary(new Rng(mix(args.seed, 0)));
  const sample = makeZipf(words);
  const folders = buildFolders(structureRng, args.notes, sample);
  const notes = buildNotes(structureRng, args.notes, folders, sample);

  mkdirSync(vault, { recursive: true });
  const createdDirs = new Set();
  const manifest = [];
  let totalBytes = 0;
  let withFrontmatter = 0;
  let withLinks = 0;
  let maxBytes = 0;
  const sizes = [];
  for (let i = 0; i < notes.length; i += 1) {
    const rng = new Rng(mix(args.seed, i + 2));
    const text = renderNote(i, notes, rng, sample);
    const bytes = Buffer.from(text, 'utf8');
    const dir = join(vault, notes[i].folder);
    if (!createdDirs.has(dir)) {
      mkdirSync(dir, { recursive: true });
      createdDirs.add(dir);
    }
    writeFileSync(join(vault, notes[i].path), bytes);
    const sha = createHash('sha256').update(bytes).digest('hex');
    manifest.push([notes[i].path, bytes.length, sha]);
    totalBytes += bytes.length;
    maxBytes = Math.max(maxBytes, bytes.length);
    sizes.push(bytes.length);
    if (text.startsWith('---\n')) withFrontmatter += 1;
    if (text.includes('[[')) withLinks += 1;
  }
  manifest.sort((a, b) => compareBytes(a[0], b[0]));
  const manifestText = `path\tbytes\tsha256\n${manifest.map((row) => row.join('\t')).join('\n')}\n`;
  writeFileSync(join(out, 'manifest.tsv'), manifestText);
  const manifestSha = createHash('sha256').update(manifestText).digest('hex');

  sizes.sort((a, b) => a - b);
  const q = (p) => sizes[Math.min(sizes.length - 1, Math.floor(p * sizes.length))];
  const basenames = new Map();
  for (const note of notes) basenames.set(note.base, (basenames.get(note.base) ?? 0) + 1);
  const duplicateBasenames = [...basenames.values()].filter((n) => n > 1).length;
  const nonAsciiPaths = notes.filter((n) => /[^\x00-\x7f]/.test(n.path)).length;
  const depths = folders.map((f) => f.split('/').length);

  const readme = `# Synthetic scale corpus (${args.notes} notes)

Generated by \`bench/scale-62k/generate.mjs\` in the Kwiry repository. All
text, names and folders are invented by a seeded PRNG; nothing is copied from
any real vault.

- Notes: ${args.notes} Markdown files under \`vault/\`
- Seed: ${args.seed}
- Generator version: ${GENERATOR_VERSION}
- Generator commit: ${generatorCommit()}
- Total bytes: ${totalBytes}
- Folders: ${folders.length} (depth ${Math.min(...depths)}-${Math.max(...depths)})
- Note size bytes: min ${sizes[0]}, p50 ${q(0.5)}, p90 ${q(0.9)}, p99 ${q(0.99)}, max ${maxBytes}
- Notes with YAML frontmatter: ${withFrontmatter}
- Notes containing wikilinks: ${withLinks}
- Basenames used by more than one note: ${duplicateBasenames}
- Note paths containing non-ASCII characters: ${nonAsciiPaths}
- \`manifest.tsv\` SHA-256: ${manifestSha}

\`manifest.tsv\` is tab-separated with a header row: \`path\` (vault-relative,
\`/\`-separated, UTF-8), \`bytes\`, \`sha256\` (lowercase hex of the file bytes),
sorted by the UTF-8 bytes of \`path\`.

Regenerate (the output directory must not exist):

\`\`\`
node bench/scale-62k/generate.mjs --notes ${args.notes} --seed ${args.seed} --out <dir>
\`\`\`
`;
  writeFileSync(join(out, 'README.md'), readme);
  process.stdout.write(`${JSON.stringify({ out, notes: args.notes, seed: args.seed, totalBytes, folders: folders.length, p50: q(0.5), max: maxBytes, manifestSha })}\n`);
}

main();
