import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, readFile, realpath, stat } from "node:fs/promises";
import { join, relative, resolve, sep } from "node:path";

export async function verifyPackedSdk({
  packageRoot,
  tarballPath,
  expectedTarballSha256,
  entryKind,
}) {
  if (entryKind !== "node" && entryKind !== "browser") throw new Error("invalid SDK entry kind");
  const root = await realpath(resolve(packageRoot));
  if (!root.endsWith(`${sep}node_modules${sep}@teslatlas${sep}sdk`)) {
    throw new Error("SDK package root must be an installed @teslatlas/sdk package");
  }
  const tarball = await realpath(resolve(tarballPath));
  const tarballSha256 = sha256(await readFile(tarball));
  if (tarballSha256 !== expectedTarballSha256) throw new Error("SDK tarball SHA-256 mismatch");
  const members = packedFileMembers(tarball);
  const installedManifest = [];
  for (const member of members) {
    const relativePath = member.slice("package/".length);
    const installedPath = join(root, relativePath);
    const installedStat = await lstat(installedPath);
    if (!installedStat.isFile() || installedStat.isSymbolicLink()) {
      throw new Error(`installed SDK member is not a regular file: ${relativePath}`);
    }
    const installedBytes = await readFile(installedPath);
    const packedBytes = readPacked(tarball, member);
    const installedSha256 = sha256(installedBytes);
    if (installedSha256 !== sha256(packedBytes)) {
      throw new Error(`installed SDK member does not match tarball: ${relativePath}`);
    }
    installedManifest.push({ path: relativePath, sha256: installedSha256 });
  }
  const packageJson = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
  const packedPackageJson = JSON.parse(
    readPacked(tarball, "package/package.json").toString("utf8"),
  );
  if (
    packageJson.name !== "@teslatlas/sdk" ||
    packageJson.version !== packedPackageJson.version ||
    packedPackageJson.name !== packageJson.name
  ) {
    throw new Error("installed SDK metadata does not match the tarball");
  }
  const entry = await realpath(join(root, `dist/${entryKind}.js`));
  if (relative(root, entry).startsWith("..")) throw new Error("SDK entry escapes package root");
  const entrySha256 = installedManifest.find(({ path }) => path === `dist/${entryKind}.js`)?.sha256;
  if (entrySha256 === undefined) throw new Error("SDK entry is absent from packed manifest");
  const installedContentManifestSha256 = sha256(
    Buffer.from(`${JSON.stringify(installedManifest)}\n`, "utf8"),
  );
  return {
    entry,
    witness: {
      packageName: packageJson.name,
      packageVersion: packageJson.version,
      entry: `dist/${entryKind}.js`,
      entrySha256,
      tarballSha256,
      installedContentManifestSha256,
      installedMemberCount: installedManifest.length,
    },
  };
}

export function assertSafeBrowserArguments(arguments_) {
  if (!Array.isArray(arguments_) || arguments_.some((value) => typeof value !== "string")) {
    throw new Error("browser arguments are invalid");
  }
  const forbidden = [
    "--ignore-certificate-errors",
    "--ignore-certificate-errors-spki-list",
    "--allow-insecure-localhost",
    "--test-type",
  ];
  if (
    arguments_.some((value) =>
      forbidden.some((flag) => value === flag || value.startsWith(`${flag}=`)),
    )
  ) {
    throw new Error("browser certificate bypass flag is forbidden");
  }
}

export async function verifyBrowserTrustWitness({
  witnessPath,
  certificatePath,
  trustedArguments,
  untrustedArguments,
}) {
  const metadata = await stat(witnessPath);
  if (
    !metadata.isFile() ||
    (metadata.mode & 0o077) !== 0 ||
    (process.getuid !== undefined && metadata.uid !== process.getuid())
  )
    throw new Error("browser trust witness must be owner-only");
  const witness = JSON.parse(await readFile(witnessPath, "utf8"));
  assertSafeBrowserArguments(trustedArguments);
  assertSafeBrowserArguments(untrustedArguments);
  if (
    witness.schemaVersion !== 1 ||
    JSON.stringify(witness.trusted?.arguments) !== JSON.stringify(trustedArguments) ||
    JSON.stringify(witness.untrusted?.arguments) !== JSON.stringify(untrustedArguments)
  ) {
    throw new Error("browser launch witness does not match live CDP arguments");
  }
  const certificateSha256 = certificateDigest(await readFile(certificatePath));
  const exportedCaSha256 = certificateDigest(await readFile(witness.trusted.nssCaExportPath));
  if (witness.certificateSha256 !== certificateSha256 || exportedCaSha256 !== certificateSha256) {
    throw new Error("browser NSS trust witness does not match fixture CA");
  }
  return { certificateSha256, trustedNssDatabase: witness.trusted.nssDatabase };
}

function readPacked(tarball, member) {
  return execFileSync("tar", ["-xOf", tarball, member], { maxBuffer: 8 * 1024 * 1024 });
}

function packedFileMembers(tarball) {
  const entries = execFileSync("tar", ["-tzf", tarball], { encoding: "utf8" })
    .split("\n")
    .filter((entry) => entry.length > 0 && !entry.endsWith("/"))
    .sort();
  if (
    entries.length === 0 ||
    entries.some(
      (entry) =>
        !entry.startsWith("package/") ||
        entry === "package/" ||
        entry.slice("package/".length).split("/").includes(".."),
    )
  ) {
    throw new Error("SDK tarball member list is invalid");
  }
  return entries;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function certificateDigest(bytes) {
  const text = bytes.toString("ascii");
  const match = text.match(
    /-----BEGIN CERTIFICATE-----([A-Za-z0-9+/=\s]+)-----END CERTIFICATE-----/u,
  );
  return sha256(match === null ? bytes : Buffer.from(match[1].replaceAll(/\s/gu, ""), "base64"));
}
