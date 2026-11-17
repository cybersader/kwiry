import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = new URL("../", import.meta.url);
const read = (name) => JSON.parse(readFileSync(new URL(name, root), "utf8"));
const manifest = read("manifest.json");
const pkg = read("package.json");
const lock = read("package-lock.json");
const versions = read("versions.json");

// BRAT supports Kwiry's existing prerelease tags as well as stable x.y.z releases.
export function checkVersions(manifest, pkg, lock, versions, tag) {
  const version = manifest.version;
  const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u;
  if (typeof version !== "string" || !semver.test(version)) {
    throw new Error("manifest.version must be a plain semantic version (BRAT prereleases allowed)");
  }
  if (typeof manifest.minAppVersion !== "string" || !/^\d+\.\d+\.\d+$/u.test(manifest.minAppVersion)) {
    throw new Error("manifest.minAppVersion must be an x.y.z version");
  }
  if (pkg.version !== version || lock.version !== version || lock.packages?.[""]?.version !== version
    || versions[version] !== manifest.minAppVersion) {
    throw new Error("package.json, package-lock.json, manifest.json and versions.json must agree");
  }
  if (tag !== undefined && tag !== version) {
    throw new Error("The release tag must equal manifest.version exactly (without v)");
  }
  return version;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const version = checkVersions(manifest, pkg, lock, versions, process.env.VERSION_TAG);
  console.log(`Plugin version ${version} is consistent`);
}
