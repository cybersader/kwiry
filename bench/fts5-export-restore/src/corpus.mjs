// Deterministic 10,000-note synthetic corpus for the SQLite export/restore
// feasibility gate, rebuilt after adversarial review:
// - Zipfian vocabulary (~80,000 types, s≈1.07) instead of a 120-word list,
//   so FTS index size is realistic rather than ~2.7x understated;
// - every note is composed as REAL Markdown text (frontmatter, headings,
//   paragraphs) and the corpus scale is the measured UTF-8 byte length of
//   that Markdown, not a self-reported estimate;
// - chunk sizes are heavy-tailed (short stub notes through long reference
//   sections) instead of a degenerate 2-3 KiB band.
// Seeded PRNG only: byte-identical across runs and processes.

const NOTE_COUNT = 10_000;
const TARGET_BYTES = 50 * 1024 * 1024;
const VOCABULARY_TYPES = 80_000;
const ZIPF_S = 1.07;

function mulberry32(seed) {
  let state = seed >>> 0;
  return function next() {
    state |= 0;
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// Deterministic surface form for vocabulary rank r: frequent ranks get
// short tokens, rare ranks long ones, mirroring natural length/frequency
// correlation. Base-26 letters only so unicode61 tokenization is exact.
function tokenForRank(rank) {
  const length = rank < 100 ? 3 + (rank % 3) : rank < 2_000 ? 5 + (rank % 3) : 7 + (rank % 5);
  let value = (Math.imul(rank + 1, 2654435761) >>> 0) + rank;
  let token = '';
  for (let index = 0; index < length; index += 1) {
    token += String.fromCharCode(97 + (value % 26));
    value = (Math.imul(value, 1103515245) + 12345) >>> 0;
  }
  return token;
}

function buildZipfSampler(random) {
  const cumulative = new Float64Array(VOCABULARY_TYPES);
  let total = 0;
  for (let rank = 0; rank < VOCABULARY_TYPES; rank += 1) {
    total += 1 / Math.pow(rank + 1, ZIPF_S);
    cumulative[rank] = total;
  }
  return function sample() {
    const target = random() * total;
    let low = 0;
    let high = VOCABULARY_TYPES - 1;
    while (low < high) {
      const middle = (low + high) >> 1;
      if (cumulative[middle] < target) low = middle + 1;
      else high = middle;
    }
    return tokenForRank(low);
  };
}

const TOPICS = [
  'architecture', 'operations', 'meetings', 'research', 'incidents',
  'designs', 'reviews', 'journal', 'reference', 'projects',
];

function fnv1a(text) {
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

function utf8Length(text) {
  // Corpus is ASCII by construction, so length == UTF-8 bytes; keep the
  // helper so any future non-ASCII change stays honestly measured.
  return Buffer.byteLength(text, 'utf8');
}

/// Heavy-tailed note size class: many small notes, some medium, few large.
function sizeClass(note, random) {
  const roll = random();
  if (roll < 0.35) return { sections: 1, paragraphs: 1, wordsPerParagraph: 36 + (note % 46) };
  if (roll < 0.8) return { sections: 1 + (note % 3), paragraphs: 3, wordsPerParagraph: 70 };
  if (roll < 0.97) return { sections: 2 + (note % 4), paragraphs: 6, wordsPerParagraph: 92 };
  return { sections: 4 + (note % 5), paragraphs: 10, wordsPerParagraph: 138 };
}

/// Streams one note at a time so the harness never holds the whole corpus
/// in memory (a production indexer streams too). Yields
/// { chunks, markdownBytes } per note; fields mirror the Gate 1 schema and
/// content is sliced from composed Markdown so scale and indexed text agree.
export function* streamNotes() {
  const random = mulberry32(0x5eed);
  const sampleWord = buildZipfSampler(random);
  const words = (count) => {
    const parts = [];
    for (let index = 0; index < count; index += 1) parts.push(sampleWord());
    return parts.join(' ');
  };

  for (let note = 0; note < NOTE_COUNT; note += 1) {
    const chunks = [];
    const topic = TOPICS[note % TOPICS.length];
    const path = `notes/${topic}/note-${String(note).padStart(5, '0')}.md`;
    const filename = path.split('/').pop();
    const stem = filename.replace(/\.md$/, '');
    const title = `${topic} ${words(3)} ${note}`;
    const tags = [topic, tokenForRank(note % 500)];
    const aliases = note % 7 === 0 ? `alias-${stem} ${words(2)}` : '';
    const sourceKey = `vault ${path}`;
    const shape = sizeClass(note, random);

    let markdown = `---\ntitle: ${title}\ntags: [${tags.join(', ')}]\n`;
    if (aliases) markdown += `aliases: [${aliases}]\n`;
    markdown += '---\n\n';

    for (let section = 0; section < shape.sections; section += 1) {
      const headingText = `${words(4)} section ${section}`;
      markdown += `## ${headingText}\n\n`;
      const paragraphs = [];
      for (let paragraph = 0; paragraph < shape.paragraphs; paragraph += 1) {
        paragraphs.push(words(shape.wordsPerParagraph));
      }
      // Deterministic markers so queries have exact expected hit sets.
      if (note % 250 === 0 && section === 0) {
        paragraphs.push(`zeta${note}term appears exactly here`);
      }
      if (note % 97 === 0) {
        paragraphs.push(`tracked as CVE-2026-${String(1000 + (note % 9000))}`);
      }
      paragraphs.push(
        `see [[note-${String((note + 1) % NOTE_COUNT).padStart(5, '0')}]]`,
      );
      const content = paragraphs.join('\n\n');
      markdown += `${content}\n\n`;

      const identifiers =
        note % 97 === 0 ? `CVE-2026-${String(1000 + (note % 9000))}` : '';
      chunks.push({
        source_key: sourceKey,
        chunk_id: `${fnv1a(path)}-${section}`,
        vault_id: 'vault',
        path,
        heading_path_json: JSON.stringify([title, headingText]),
        frontmatter_json: JSON.stringify({ title, tags }),
        mtime: 1_700_000_000 + note,
        content_hash: fnv1a(content),
        chunking_version: 1,
        filename,
        stem,
        aliases,
        title,
        heading_text: headingText,
        path_text: path.replaceAll('/', ' '),
        tags: tags.join(' '),
        content,
        identifiers,
      });
    }
    yield { chunks, markdownBytes: utf8Length(markdown) };
  }
}

export const CORPUS_META = Object.freeze({
  noteCount: NOTE_COUNT,
  targetBytes: TARGET_BYTES,
});
