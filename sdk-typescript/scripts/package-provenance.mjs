import { createHash } from "node:crypto";
import { lstat, readdir, readFile, realpath } from "node:fs/promises";
import { basename, isAbsolute, join, relative, resolve } from "node:path";
import { gunzipSync } from "node:zlib";

const HEX_40 = /^[0-9a-f]{40}$/u;
const HEX_64 = /^[0-9a-f]{64}$/u;
const PRODUCT_VERSION = /^[0-9]{4}\.[0-9]{1,2}\.[0-9]+$/u;
const TOOL_VERSION = /^[0-9]+\.[0-9]+\.[0-9]+$/u;
const HISTORICAL_ARCHIVE_SHA256 =
  "42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7";
const PROFILE_SHA256 = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926";
const EDGE_PROFILE_SHA256 = "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224";
const SDK_REPOSITORY = "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git";
const REPOSITORIES = {
  protocol: "https://github.com/magrathean-uk/teslatlas-protocol.git",
  "sdk-typescript": SDK_REPOSITORY,
  "sdk-swift": "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
  "home-assistant": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
  edge: "https://github.com/magrathean-uk/teslatlas-edge.git",
};
const COMPONENTS = ["edge", "home-assistant", "protocol", "sdk-swift", "sdk-typescript"];

export function requireSha256(value, label) {
  if (!HEX_64.test(value ?? "")) throw new Error(`${label} must be a lowercase SHA-256`);
  return value;
}

export function requireCommit(value, label) {
  if (!HEX_40.test(value ?? "")) throw new Error(`${label} must be a full lowercase commit`);
  return value;
}

export async function canonicalRegularFile(filePath, label) {
  return canonicalPath(filePath, label, "file");
}

export async function canonicalDirectory(directoryPath, label) {
  return canonicalPath(directoryPath, label, "directory");
}

async function canonicalPath(inputPath, label, kind) {
  if (typeof inputPath !== "string" || !isAbsolute(inputPath) || resolve(inputPath) !== inputPath) {
    throw new Error(`${label} must be an absolute canonical path`);
  }
  let canonical;
  try {
    canonical = await realpath(inputPath);
  } catch {
    throw new Error(`${label} is unavailable`);
  }
  if (canonical !== inputPath) throw new Error(`${label} must not be a symlink`);
  const metadata = await lstat(canonical);
  const validKind = kind === "file" ? metadata.isFile() : metadata.isDirectory();
  if (!validKind || metadata.isSymbolicLink() || (kind === "file" && metadata.nlink !== 1)) {
    throw new Error(`${label} must be a regular single-link ${kind}`);
  }
  const mode = metadata.mode & 0o7777;
  if (
    (mode & 0o7022) !== 0 ||
    (mode & 0o400) === 0 ||
    (kind === "directory" && (mode & 0o100) === 0)
  ) {
    throw new Error(`${label} has unsafe permissions`);
  }
  return canonical;
}

export async function sha256File(filePath) {
  return sha256(await readFile(filePath));
}

export function parseStrictJson(source, label = "JSON") {
  if (typeof source !== "string") throw new Error(`${label} must be UTF-8 text`);
  let position = 0;
  const fail = (message) => {
    throw new Error(`${label} ${message} at byte ${position}`);
  };
  const whitespace = () => {
    while ([" ", "\t", "\n", "\r"].includes(source[position] ?? "")) position += 1;
  };
  const string = () => {
    if (source[position] !== '"') fail("contains invalid JSON");
    const start = position;
    position += 1;
    while (position < source.length) {
      const character = source[position];
      if (character === '"') {
        position += 1;
        try {
          return JSON.parse(source.slice(start, position));
        } catch {
          fail("contains an invalid string");
        }
      }
      if (character === "\\") {
        position += 2;
      } else {
        if ((character?.codePointAt(0) ?? 0) < 0x20) fail("contains a control character");
        position += 1;
      }
    }
    fail("contains an unterminated string");
  };
  const value = () => {
    whitespace();
    const character = source[position];
    if (character === '"') return string();
    if (character === "{") {
      position += 1;
      const result = {};
      const keys = new Set();
      whitespace();
      if (source[position] === "}") {
        position += 1;
        return result;
      }
      while (position < source.length) {
        whitespace();
        const key = string();
        if (keys.has(key))
          throw new Error(`${label} contains duplicate key ${JSON.stringify(key)}`);
        keys.add(key);
        whitespace();
        if (source[position] !== ":") fail("contains invalid object syntax");
        position += 1;
        Object.defineProperty(result, key, {
          configurable: true,
          enumerable: true,
          value: value(),
          writable: true,
        });
        whitespace();
        if (source[position] === "}") {
          position += 1;
          return result;
        }
        if (source[position] !== ",") fail("contains invalid object syntax");
        position += 1;
      }
      fail("contains an unterminated object");
    }
    if (character === "[") {
      position += 1;
      const result = [];
      whitespace();
      if (source[position] === "]") {
        position += 1;
        return result;
      }
      while (position < source.length) {
        result.push(value());
        whitespace();
        if (source[position] === "]") {
          position += 1;
          return result;
        }
        if (source[position] !== ",") fail("contains invalid array syntax");
        position += 1;
      }
      fail("contains an unterminated array");
    }
    const rest = source.slice(position);
    const token = rest.match(
      /^(?:true|false|null|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)/u,
    )?.[0];
    if (token === undefined) fail("contains invalid JSON");
    position += token.length;
    return JSON.parse(token);
  };
  const result = value();
  whitespace();
  if (position !== source.length) fail("contains trailing data");
  return result;
}

export async function readStrictJsonFile(filePath, label, expectedSha256) {
  const path = await canonicalRegularFile(filePath, label);
  const bytes = await readFile(path);
  const observedSha256 = sha256(bytes);
  if (
    expectedSha256 !== undefined &&
    observedSha256 !== requireSha256(expectedSha256, `${label} SHA-256`)
  ) {
    throw new Error(`${label} SHA-256 mismatch`);
  }
  return { path, sha256: observedSha256, value: parseStrictJson(decodeUtf8(bytes, label), label) };
}

export async function inspectPackageArchive(filePath, expectedSha256, label = "SDK archive") {
  const path = await canonicalRegularFile(filePath, label);
  const bytes = await readFile(path);
  const observedSha256 = sha256(bytes);
  if (observedSha256 !== requireSha256(expectedSha256, `${label} SHA-256`)) {
    throw new Error(`${label} SHA-256 mismatch`);
  }
  let tar;
  try {
    tar = gunzipSync(bytes, { maxOutputLength: 64 * 1024 * 1024 });
  } catch {
    throw new Error(`${label} is not a bounded gzip archive`);
  }
  const members = parseTar(tar, label);
  const packageJson = members.find((member) => member.path === "package/package.json");
  if (packageJson === undefined || packageJson.type !== "file") {
    throw new Error(`${label} package metadata is absent`);
  }
  const manifest = parseStrictJson(
    decodeUtf8(packageJson.bytes, `${label} package metadata`),
    `${label} package metadata`,
  );
  if (
    manifest?.name !== "@teslatlas/sdk" ||
    !PRODUCT_VERSION.test(manifest.version ?? "") ||
    manifest.private !== true
  ) {
    throw new Error(`${label} package metadata is invalid`);
  }
  const expectedFilename = `teslatlas-sdk-${manifest.version}.tgz`;
  if (basename(path) !== expectedFilename)
    throw new Error(`${label} filename must be ${expectedFilename}`);
  return {
    path,
    sha256: observedSha256,
    memberCount: members.filter((member) => member.type === "file").length,
    packageName: manifest.name,
    packageVersion: manifest.version,
    packageManager: manifest.packageManager ?? null,
  };
}

function parseTar(tar, label) {
  const members = [];
  const paths = new Set();
  let offset = 0;
  let zeroBlocks = 0;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) {
      zeroBlocks += 1;
      offset += 512;
      if (zeroBlocks === 2) break;
      continue;
    }
    zeroBlocks = 0;
    verifyTarChecksum(header, label);
    const name = tarText(header.subarray(0, 100), label);
    const prefix = tarText(header.subarray(345, 500), label);
    const rawPath = prefix.length === 0 ? name : `${prefix}/${name}`;
    const typeFlag = header[156] === 0 ? "0" : String.fromCodePoint(header[156]);
    const type = typeFlag === "0" ? "file" : typeFlag === "5" ? "directory" : undefined;
    if (type === undefined) throw new Error(`${label} contains a link or special entry`);
    const path = safeArchivePath(rawPath, type, label);
    if (paths.has(path)) throw new Error(`${label} contains duplicate member ${path}`);
    paths.add(path);
    const mode = tarOctal(header.subarray(100, 108), `${label} mode`);
    if (
      (mode & 0o7022) !== 0 ||
      (mode & 0o400) === 0 ||
      (type === "directory" && (mode & 0o100) === 0)
    ) {
      throw new Error(`${label} contains unsafe mode for ${path}`);
    }
    const size = tarOctal(header.subarray(124, 136), `${label} size`);
    if (type === "directory" && size !== 0) throw new Error(`${label} directory has payload bytes`);
    const dataStart = offset + 512;
    const dataEnd = dataStart + size;
    const next = dataStart + Math.ceil(size / 512) * 512;
    if (!Number.isSafeInteger(size) || dataEnd > tar.length || next > tar.length) {
      throw new Error(`${label} member size exceeds the archive`);
    }
    members.push({ path, type, mode, bytes: Buffer.from(tar.subarray(dataStart, dataEnd)) });
    if (members.length > 10_000) throw new Error(`${label} has too many members`);
    offset = next;
  }
  if (zeroBlocks < 2 || tar.subarray(offset).some((byte) => byte !== 0)) {
    throw new Error(`${label} has invalid trailing data`);
  }
  if (members.length === 0) throw new Error(`${label} has no members`);
  return members;
}

function verifyTarChecksum(header, label) {
  const expected = tarOctal(header.subarray(148, 156), `${label} checksum`);
  let observed = 0;
  for (let index = 0; index < header.length; index += 1) {
    observed += index >= 148 && index < 156 ? 0x20 : header[index];
  }
  if (observed !== expected) throw new Error(`${label} has an invalid tar checksum`);
}

function tarOctal(bytes, label) {
  const text = Buffer.from(bytes).toString("ascii").replaceAll("\0", "").trim();
  if (!/^[0-7]+$/u.test(text)) throw new Error(`${label} is not portable octal`);
  return Number.parseInt(text, 8);
}

function tarText(bytes, label) {
  const end = bytes.indexOf(0);
  const value = bytes.subarray(0, end === -1 ? bytes.length : end);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(value);
  } catch {
    throw new Error(`${label} contains a non-UTF-8 path`);
  }
}

function decodeUtf8(bytes, label) {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error(`${label} is not valid UTF-8`);
  }
}

function safeArchivePath(rawPath, type, label) {
  const path = type === "directory" ? rawPath.replace(/\/$/u, "") : rawPath;
  const parts = path.split("/");
  if (
    path.length === 0 ||
    path.startsWith("/") ||
    path.includes("\\") ||
    parts.some((part) => part.length === 0 || part === "." || part === "..") ||
    (path !== "package" && !path.startsWith("package/")) ||
    (type === "file" && path === "package")
  ) {
    throw new Error(`${label} contains unsafe path ${JSON.stringify(rawPath)}`);
  }
  return path;
}

export async function sourceExportManifest(sourceExport, label = "source export") {
  const root = await canonicalDirectory(sourceExport, label);
  const entries = [];
  await walkSource(root, root, entries, label);
  if (entries.length === 0) throw new Error(`${label} contains no files`);
  const manifestBytes = Buffer.from(`${JSON.stringify(entries)}\n`, "utf8");
  return { root, fileCount: entries.length, manifestSha256: sha256(manifestBytes) };
}

async function walkSource(root, directory, entries, label) {
  const children = await readdir(directory, { withFileTypes: true });
  children.sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));
  for (const child of children) {
    const path = join(directory, child.name);
    const relativePath = relative(root, path).replaceAll("\\", "/");
    if (
      child.name === ".git" ||
      child.name === ".DS_Store" ||
      child.name === ".serena" ||
      child.name === "node_modules" ||
      child.name === "dist" ||
      child.name.endsWith(".tgz") ||
      relativePath.split("/").some((part) => part === ".." || part.length === 0)
    ) {
      throw new Error(`${label} contains forbidden path ${relativePath}`);
    }
    const metadata = await lstat(path);
    if (metadata.isSymbolicLink() || (!metadata.isFile() && !metadata.isDirectory())) {
      throw new Error(`${label} contains a link or special entry`);
    }
    const mode = metadata.mode & 0o7777;
    if (
      (mode & 0o7022) !== 0 ||
      (mode & 0o400) === 0 ||
      (metadata.isDirectory() && (mode & 0o100) === 0)
    ) {
      throw new Error(`${label} contains unsafe mode for ${relativePath}`);
    }
    if (metadata.isDirectory()) await walkSource(root, path, entries, label);
    else entries.push({ path: relativePath, mode, sha256: sha256(await readFile(path)) });
  }
}

export async function validateReviewedPackageBinding({
  role,
  receiptPath,
  receiptSha256,
  archive,
  sourceExport,
}) {
  const receiptFile = await readStrictJsonFile(receiptPath, `${role} receipt`, receiptSha256);
  const receipt = receiptFile.value;
  const source = await sourceExportManifest(sourceExport, `${role} source export`);
  validateExactKeys(
    receipt,
    [
      "schema_version",
      "kind",
      "role",
      "state",
      "product_version",
      "source",
      "package",
      "toolchain",
      "review",
    ],
    `${role} receipt`,
  );
  validateExactKeys(
    receipt.source,
    ["repository", "commit", "manifest_sha256", "file_count"],
    `${role} source receipt`,
  );
  validateExactKeys(
    receipt.package,
    ["name", "version", "archive_sha256", "member_count"],
    `${role} package receipt`,
  );
  validateExactKeys(receipt.toolchain, ["node", "npm"], `${role} toolchain receipt`);
  validateExactKeys(
    receipt.review,
    ["verdict", "reviewer_model", "reasoning", "reviewed_at"],
    `${role} review receipt`,
  );
  if (
    receipt.schema_version !== 1 ||
    receipt.kind !== "teslatlas.sdk-package-admission" ||
    receipt.role !== role ||
    receipt.state !== "independently-reviewed" ||
    receipt.review.verdict !== "ACCEPT" ||
    receipt.review.reviewer_model !== "GPT-5.6 Sol" ||
    receipt.review.reasoning !== "high" ||
    typeof receipt.review.reviewed_at !== "string" ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/u.test(receipt.review.reviewed_at) ||
    Number.isNaN(Date.parse(receipt.review.reviewed_at)) ||
    receipt.source.repository !== SDK_REPOSITORY ||
    !HEX_40.test(receipt.source.commit ?? "") ||
    receipt.source.manifest_sha256 !== source.manifestSha256 ||
    receipt.source.file_count !== source.fileCount ||
    receipt.product_version !== archive.packageVersion ||
    receipt.package.name !== archive.packageName ||
    receipt.package.version !== archive.packageVersion ||
    receipt.package.archive_sha256 !== archive.sha256 ||
    receipt.package.member_count !== archive.memberCount ||
    !TOOL_VERSION.test(receipt.toolchain.node ?? "") ||
    !TOOL_VERSION.test(receipt.toolchain.npm ?? "")
  ) {
    throw new Error(`${role} receipt does not bind accepted source/package/toolchain evidence`);
  }
  if (
    role === "candidate" &&
    (archive.sha256 === HISTORICAL_ARCHIVE_SHA256 ||
      receipt.package.archive_sha256 === HISTORICAL_ARCHIVE_SHA256)
  ) {
    throw new Error("historical accepted archive cannot represent the changed candidate");
  }
  if (
    role === "candidate" &&
    (receipt.toolchain.node !== "26.7.0" || receipt.toolchain.npm !== "11.19.0")
  ) {
    throw new Error("candidate receipt requires exact Node 26.7.0 and npm 11.19.0");
  }
  return {
    role,
    receiptPath: receiptFile.path,
    receiptSha256: receiptFile.sha256,
    receipt,
    source,
  };
}

export async function readCatalogBinding({ catalogPath, catalogSha256, reviewedPackage }) {
  const catalogFile = await readStrictJsonFile(catalogPath, "catalog", catalogSha256);
  const catalog = catalogFile.value;
  validateExactKeys(catalog, ["schema_version", "cohorts"], "catalog");
  if (
    catalog.schema_version !== 1 ||
    !Array.isArray(catalog.cohorts) ||
    catalog.cohorts.length === 0
  ) {
    throw new Error("catalog schema is unsupported or empty");
  }
  const versions = new Set();
  for (const cohort of catalog.cohorts) {
    validateCohort(cohort);
    if (versions.has(cohort.product_version))
      throw new Error("catalog has duplicate cohort version");
    versions.add(cohort.product_version);
  }
  const version = reviewedPackage.receipt.product_version;
  const matches = catalog.cohorts.filter((cohort) => cohort.product_version === version);
  if (matches.length !== 1) throw new Error("catalog must contain exactly one package cohort");
  const cohort = matches[0];
  const component = cohort.components["sdk-typescript"];
  if (
    component.commit !== reviewedPackage.receipt.source.commit ||
    component.source_sha256 !== reviewedPackage.source.manifestSha256 ||
    component.artifacts.package_sha256 !== reviewedPackage.receipt.package.archive_sha256
  ) {
    throw new Error("catalog TypeScript source/package admission does not match exact inputs");
  }
  return {
    path: catalogFile.path,
    sha256: catalogFile.sha256,
    publicationStatus: cohort.publication_status,
    admittedHubVersions: cohort.admitted_hub_versions,
    sourceCommit: component.commit,
    sourceSha256: component.source_sha256,
    packageSha256: component.artifacts.package_sha256,
    packageMemberCount: reviewedPackage.receipt.package.member_count,
    admissionReceiptSha256: reviewedPackage.receiptSha256,
    profile: component.profile,
  };
}

function validateCohort(cohort) {
  validateExactKeys(
    cohort,
    ["product_version", "publication_status", "admitted_hub_versions", "components"],
    "catalog cohort",
  );
  if (
    !PRODUCT_VERSION.test(cohort.product_version ?? "") ||
    !["local-unpublished", "published"].includes(cohort.publication_status) ||
    !Array.isArray(cohort.admitted_hub_versions) ||
    cohort.admitted_hub_versions.length === 0 ||
    new Set(cohort.admitted_hub_versions).size !== cohort.admitted_hub_versions.length ||
    cohort.admitted_hub_versions.some((version) => !PRODUCT_VERSION.test(version))
  ) {
    throw new Error("catalog cohort metadata is invalid");
  }
  validateExactKeys(cohort.components, COMPONENTS, "catalog components");
  for (const name of COMPONENTS)
    validateComponent(name, cohort.components[name], cohort.product_version);
}

function validateComponent(name, component, version) {
  const artifactKeys =
    name === "sdk-typescript"
      ? ["package_filename", "package_sha256"]
      : name === "home-assistant"
        ? ["payload_manifest_sha256", "selection_receipt_sha256"]
        : [];
  const componentKeys = ["repository", "commit", "source_sha256", "product_version", "profile"];
  if (artifactKeys.length > 0) componentKeys.push("artifacts");
  validateExactKeys(component, componentKeys, `catalog ${name}`);
  const expectedProfile =
    name === "edge"
      ? { id: "edge-delivery-v2", revision: "2.0.0", sha256: EDGE_PROFILE_SHA256 }
      : { id: "hub-http-v1", revision: "1.0.0", sha256: PROFILE_SHA256 };
  validateExactKeys(component.profile, ["id", "revision", "sha256"], `catalog ${name} profile`);
  if (
    component.repository !== REPOSITORIES[name] ||
    !HEX_40.test(component.commit ?? "") ||
    !HEX_64.test(component.source_sha256 ?? "") ||
    component.product_version !== version ||
    JSON.stringify(component.profile) !== JSON.stringify(expectedProfile)
  ) {
    throw new Error(`catalog ${name} identity is invalid`);
  }
  if (artifactKeys.length === 0) return;
  validateExactKeys(component.artifacts, artifactKeys, `catalog ${name} artifacts`);
  if (name === "sdk-typescript") {
    if (
      component.artifacts.package_filename !== `teslatlas-sdk-${version}.tgz` ||
      !HEX_64.test(component.artifacts.package_sha256 ?? "")
    ) {
      throw new Error("catalog sdk-typescript artifacts are invalid");
    }
  } else if (
    !HEX_64.test(component.artifacts.payload_manifest_sha256 ?? "") ||
    !HEX_64.test(component.artifacts.selection_receipt_sha256 ?? "")
  ) {
    throw new Error("catalog home-assistant artifacts are invalid");
  }
}

function validateExactKeys(value, expected, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    throw new Error(`${label} fields mismatch`);
  }
}

export function compareProductVersions(predecessor, candidate) {
  if (!PRODUCT_VERSION.test(predecessor ?? "") || !PRODUCT_VERSION.test(candidate ?? "")) {
    throw new Error("package versions must use YEAR.WEEK.REVISION");
  }
  const predecessorParts = predecessor.split(".").map(Number);
  const candidateParts = candidate.split(".").map(Number);
  for (let index = 0; index < 3; index += 1) {
    if (predecessorParts[index] < candidateParts[index]) return -1;
    if (predecessorParts[index] > candidateParts[index]) return 1;
  }
  return 0;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export const historicalArchiveSha256 = HISTORICAL_ARCHIVE_SHA256;
