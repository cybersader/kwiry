// SPDX-License-Identifier: GPL-3.0-only

const { mkdirSync, rmSync, writeFileSync } = require("node:fs");
const { join, resolve } = require("node:path");
const { spawnSync } = require("node:child_process");

const adapterRoot = resolve(__dirname, "..");
const obsidianRoot = resolve(adapterRoot, "../..");
const workRoot = join(obsidianRoot, ".tmp", "docx-integration-spike", "parity");
const bindingsRoot = join(workRoot, "pkg");
const fixturePath = join(workRoot, "fixture.json");
const manifest = join(adapterRoot, "Cargo.toml");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? adapterRoot,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`${command} failed\n${result.stdout}\n${result.stderr}`);
  }
  return result.stdout.trim();
}

const crcTable = (() => {
  const table = new Uint32Array(256);
  for (let value = 0; value < 256; value += 1) {
    let crc = value;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc & 1) !== 0 ? (0xedb88320 ^ (crc >>> 1)) : (crc >>> 1);
    }
    table[value] = crc >>> 0;
  }
  return table;
})();

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const byte of buffer) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function u16(value) {
  const buffer = Buffer.alloc(2);
  buffer.writeUInt16LE(value, 0);
  return buffer;
}

function u32(value) {
  const buffer = Buffer.alloc(4);
  buffer.writeUInt32LE(value >>> 0, 0);
  return buffer;
}

function buildStoredZip(entries) {
  const local = [];
  const central = [];
  let offset = 0;
  for (const [name, text] of entries) {
    const nameBytes = Buffer.from(name, "utf8");
    const content = Buffer.from(text, "utf8");
    const crc = crc32(content);
    const localHeader = Buffer.concat([
      u32(0x04034b50), u16(20), u16(1 << 11), u16(0), u16(0), u16(0),
      u32(crc), u32(content.length), u32(content.length), u16(nameBytes.length), u16(0),
      nameBytes,
    ]);
    local.push(localHeader, content);
    central.push(Buffer.concat([
      u32(0x02014b50), u16(20), u16(20), u16(1 << 11), u16(0), u16(0), u16(0),
      u32(crc), u32(content.length), u32(content.length), u16(nameBytes.length),
      u16(0), u16(0), u16(0), u16(0), u32(0), u32(offset), nameBytes,
    ]));
    offset += localHeader.length + content.length;
  }
  const directory = Buffer.concat(central);
  return Buffer.concat([
    ...local,
    directory,
    u32(0x06054b50), u16(0), u16(0), u16(entries.length), u16(entries.length),
    u32(directory.length), u32(offset), u16(0),
  ]);
}

const contentTypes = `<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>`;
const rootRelationships = `<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="main" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="core" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>`;
const document = `<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:pPr><w:outlineLvl w:val="0"/></w:pPr><w:r><w:t>Parity heading</w:t></w:r></w:p><w:p><w:r><w:t>Visible parity</w:t></w:r><w:del><w:r><w:delText>Deleted parity</w:delText></w:r></w:del><w:r><w:rPr><w:vanish/></w:rPr><w:t>Hidden parity</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="header"/></w:sectPr></w:body></w:document>`;
const documentRelationships = `<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="header" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="comments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>`;
const header = `<?xml version="1.0" encoding="UTF-8"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Supporting header</w:t></w:r></w:p></w:hdr>`;
const comments = `<?xml version="1.0" encoding="UTF-8"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="1"><w:p><w:r><w:t>Supporting comment</w:t></w:r></w:p></w:comment></w:comments>`;
const core = `<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Parity title</dc:title><cp:keywords>one; two, three</cp:keywords></cp:coreProperties>`;
const docx = buildStoredZip([
  ["word/comments.xml", comments],
  ["docProps/core.xml", core],
  ["word/header1.xml", header],
  ["word/_rels/document.xml.rels", documentRelationships],
  ["word/document.xml", document],
  ["_rels/.rels", rootRelationships],
  ["[Content_Types].xml", contentTypes],
]);

rmSync(workRoot, { recursive: true, force: true });
mkdirSync(bindingsRoot, { recursive: true });
run("cargo", ["build", "--manifest-path", manifest, "--release", "--bin", "kwiry-obsidian-wasm-fixtures", "--features", "internal-docx-extractor"]);
run("cargo", ["build", "--manifest-path", manifest, "--release", "--target", "wasm32-unknown-unknown", "--lib", "--features", "internal-docx-extractor"]);
run("wasm-bindgen", [
  join(adapterRoot, "target", "wasm32-unknown-unknown", "release", "kwiry_obsidian_wasm.wasm"),
  "--target", "nodejs", "--out-dir", bindingsRoot, "--out-name", "kwiry_obsidian_wasm",
]);
const wasm = require(join(bindingsRoot, "kwiry_obsidian_wasm.js"));

for (const scope of ["current_view", "all_content"]) {
  const request = { abi_version: 3, operation: "internal_docx_extract", scope };
  const fixture = [{
    operation: "internal_docx_extract",
    name: scope,
    request,
    content: { encoding: "bytes", values: [...docx] },
  }];
  writeFileSync(fixturePath, JSON.stringify(fixture));
  const native = run("cargo", [
    "run", "--quiet", "--manifest-path", manifest, "--release", "--bin",
    "kwiry-obsidian-wasm-fixtures", "--features", "internal-docx-extractor", "--",
    fixturePath, "--adapter-output-only",
  ]);
  const portable = wasm.internal_docx_extract(JSON.stringify(request), Uint8Array.from(docx));
  if (native !== portable) throw new Error(`${scope} native/WASM DOCX output differs`);
  if (portable !== wasm.internal_docx_extract(JSON.stringify(request), Uint8Array.from(docx))) {
    throw new Error(`${scope} repeated WASM DOCX output differs`);
  }
  const parsed = JSON.parse(portable);
  if (parsed.status !== "ok") throw new Error(`${scope} adapter returned an error`);
  const candidate = parsed.result;
  if (candidate.properties.title !== "Parity title") throw new Error("DOCX title was not preserved");
  if (candidate.sections.some((section) => section.source_locator !== undefined)) {
    throw new Error("DOCX emitted an unsupported locator");
  }
  if (scope === "current_view" && candidate.sections.some((section) => section.role === "latent")) {
    throw new Error("current_view emitted latent DOCX content");
  }
  if (scope === "all_content" && !candidate.sections.some((section) => section.role === "latent")) {
    throw new Error("all_content omitted latent DOCX content");
  }
}

console.log("DOCX native/WASM parity passed for current_view and all_content");
