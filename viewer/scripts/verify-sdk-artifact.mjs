import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstat, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function readPacked(tarball, member) {
  return execFileSync('tar', ['-xOf', tarball, member], {
    maxBuffer: 16 * 1024 * 1024,
  });
}

function packedMembers(tarball) {
  const members = execFileSync('tar', ['-tzf', tarball], { encoding: 'utf8' })
    .split('\n')
    .filter((member) => member.length > 0 && !member.endsWith('/'))
    .sort();
  if (
    members.length === 0 ||
    members.some(
      (member) =>
        !member.startsWith('package/') ||
        member.slice('package/'.length).split('/').includes('..'),
    )
  ) {
    throw new Error('vendored SDK tarball contains an invalid member');
  }
  return members;
}

export async function verifySdkArtifact({
  projectRoot,
  packageRoot = join(projectRoot, 'node_modules/@teslatlas/sdk'),
} = {}) {
  if (!projectRoot) throw new Error('projectRoot is required');
  const metadata = JSON.parse(
    await readFile(join(projectRoot, 'artifacts/teslatlas-sdk.json'), 'utf8'),
  );
  if (
    metadata.schema_version !== 1 ||
    metadata.package_name !== '@teslatlas/sdk' ||
    metadata.package_version !== '2026.36.2' ||
    metadata.profile?.id !== 'hub-http-v1' ||
    metadata.profile?.revision !== '1.0.0'
  ) {
    throw new Error('vendored SDK metadata is invalid');
  }

  const tarball = resolve(projectRoot, metadata.tarball);
  const tarballSha256 = sha256(await readFile(tarball));
  if (tarballSha256 !== metadata.tarball_sha256) {
    throw new Error('vendored SDK tarball SHA-256 mismatch');
  }

  const packageJson = JSON.parse(await readFile(join(projectRoot, 'package.json'), 'utf8'));
  const packageLock = JSON.parse(
    await readFile(join(projectRoot, 'package-lock.json'), 'utf8'),
  );
  const exactDependency = `file:${metadata.tarball}`;
  const declaredDependency =
    packageJson.dependencies?.[metadata.package_name] ??
    packageJson.devDependencies?.[metadata.package_name];
  const lockedDependency =
    packageLock.packages?.['']?.dependencies?.[metadata.package_name] ??
    packageLock.packages?.['']?.devDependencies?.[metadata.package_name];
  if (
    declaredDependency !== exactDependency ||
    lockedDependency !== exactDependency
  ) {
    throw new Error('SDK dependency is not the verified project-relative tarball');
  }

  const packedPackageJson = JSON.parse(
    readPacked(tarball, 'package/package.json').toString('utf8'),
  );
  const installedPackageJson = JSON.parse(
    await readFile(join(packageRoot, 'package.json'), 'utf8'),
  );
  if (
    packedPackageJson.name !== metadata.package_name ||
    packedPackageJson.version !== metadata.package_version ||
    installedPackageJson.name !== metadata.package_name ||
    installedPackageJson.version !== metadata.package_version
  ) {
    throw new Error('installed SDK metadata does not match the vendored tarball');
  }

  const installedManifest = [];
  for (const member of packedMembers(tarball)) {
    const relativePath = member.slice('package/'.length);
    const installedPath = join(packageRoot, relativePath);
    const metadataForPath = await lstat(installedPath);
    if (!metadataForPath.isFile() || metadataForPath.isSymbolicLink()) {
      throw new Error(`installed SDK member is not a regular file: ${relativePath}`);
    }
    const installedBytes = await readFile(installedPath);
    const installedSha256 = sha256(installedBytes);
    if (installedSha256 !== sha256(readPacked(tarball, member))) {
      throw new Error(
        `installed SDK member does not match the vendored tarball: ${relativePath}`,
      );
    }
    installedManifest.push({ path: relativePath, sha256: installedSha256 });
  }

  const installedContentManifestSha256 = sha256(
    Buffer.from(`${JSON.stringify(installedManifest)}\n`, 'utf8'),
  );
  if (
    installedManifest.length !== metadata.installed_member_count ||
    installedContentManifestSha256 !==
      metadata.installed_content_manifest_sha256
  ) {
    throw new Error('installed SDK content manifest does not match metadata');
  }
  return {
    tarballSha256,
    installedContentManifestSha256,
    installedMemberCount: installedManifest.length,
  };
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  const result = await verifySdkArtifact({
    projectRoot: resolve(import.meta.dirname, '..'),
  });
  process.stdout.write(
    `Verified @teslatlas/sdk 2026.36.2 (${result.installedMemberCount} files, ${result.tarballSha256})\n`,
  );
}
