import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, rename, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { normalizeNpmPackReport } from './npm-pack-report.mjs';

const execFileAsync = promisify(execFile);
const requiredNodeVersion = 'v26.10.0';
const requiredNpmVersion = '12.1.0';

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function isInside(root, path) {
  return path === root || path.startsWith(`${root}${sep}`);
}

async function run(executable, arguments_, options) {
  return execFileAsync(executable, arguments_, {
    cwd: options.cwd,
    env: options.env,
    maxBuffer: 16 * 1024 * 1024,
  });
}

async function readMetadata(projectRoot) {
  const metadata = JSON.parse(
    await readFile(join(projectRoot, 'artifacts/teslatlas-sdk.json'), 'utf8'),
  );
  if (
    metadata.schema_version !== 1 ||
    metadata.package_name !== '@teslatlas/sdk' ||
    metadata.package_version !== '2026.36.2' ||
    typeof metadata.tarball !== 'string' ||
    typeof metadata.tarball_sha256 !== 'string' ||
    !/^[0-9a-f]{40}$/u.test(metadata.source?.commit ?? '') ||
    metadata.source?.node_version !== requiredNodeVersion ||
    metadata.source?.npm_version !== requiredNpmVersion
  ) {
    throw new Error('SDK preparation metadata is invalid');
  }
  const destination = resolve(projectRoot, metadata.tarball);
  if (!isInside(projectRoot, destination)) {
    throw new Error('SDK preparation tarball must stay inside the Viewer project');
  }
  return { metadata, destination };
}

function sourceFilter(path) {
  return !path.split(sep).some((segment) =>
    ['.git', '.DS_Store', 'dist', 'node_modules'].includes(segment));
}

export async function prepareSdkArtifact({ projectRoot, sdkSource, nodeExecutable }) {
  if (!projectRoot || !sdkSource || !nodeExecutable) {
    throw new Error('projectRoot, sdkSource, and nodeExecutable are required');
  }
  const root = resolve(projectRoot);
  const source = resolve(sdkSource);
  const node = resolve(nodeExecutable);
  const npm = join(dirname(node), 'npm');
  const { metadata, destination } = await readMetadata(root);
  const [sourceMetadata, nodeMetadata, npmMetadata] = await Promise.all([
    stat(source),
    stat(node),
    stat(npm),
  ]);
  if (!sourceMetadata.isDirectory() || !nodeMetadata.isFile() || !npmMetadata.isFile()) {
    throw new Error('SDK source or required Node/npm executable is unavailable');
  }

  const baseEnvironment = {
    ...process.env,
    PATH: `${dirname(node)}:${process.env.PATH ?? ''}`,
  };
  const [nodeVersion, npmVersion] = await Promise.all([
    run(node, ['--version'], { env: baseEnvironment }),
    run(npm, ['--version'], { env: baseEnvironment }),
  ]);
  if (nodeVersion.stdout.trim() !== requiredNodeVersion || npmVersion.stdout.trim() !== requiredNpmVersion) {
    throw new Error(`SDK preparation requires Node ${requiredNodeVersion} and npm ${requiredNpmVersion}`);
  }
  const sourceCommit = await run('git', ['-C', source, 'rev-parse', 'HEAD'], {
    env: baseEnvironment,
  });
  if (sourceCommit.stdout.trim() !== metadata.source.commit) {
    throw new Error('SDK source checkout does not match the Viewer metadata commit');
  }

  const temporary = await mkdtemp(join(tmpdir(), 'teslatlas-viewer-sdk-build-'));
  try {
    const sourceCopy = join(temporary, 'source');
    const cache = join(temporary, 'npm-cache');
    const packDirectory = join(temporary, 'pack');
    await cp(source, sourceCopy, { recursive: true, filter: sourceFilter });
    await mkdir(cache);
    await mkdir(packDirectory);
    const buildEnvironment = { ...baseEnvironment, npm_config_cache: cache };
    await run(npm, ['ci'], { cwd: sourceCopy, env: buildEnvironment });
    await run(npm, ['run', 'build'], { cwd: sourceCopy, env: buildEnvironment });
    const packed = await run(
      npm,
      ['pack', '--json', '--pack-destination', packDirectory],
      { cwd: sourceCopy, env: buildEnvironment },
    );
    const report = normalizeNpmPackReport(JSON.parse(packed.stdout), metadata.package_name);
    if (
      report.version !== metadata.package_version ||
      report.filename !== 'teslatlas-sdk-2026.36.2.tgz' ||
      report.entryCount !== metadata.installed_member_count
    ) {
      throw new Error('SDK source pack report is invalid');
    }
    const sourceArchive = join(packDirectory, report.filename);
    const archiveBytes = await readFile(sourceArchive);
    const tarballSha256 = sha256(archiveBytes);
    if (tarballSha256 !== metadata.tarball_sha256) {
      throw new Error('SDK source pack SHA-256 does not match Viewer metadata');
    }
    await mkdir(dirname(destination), { recursive: true });
    const stagedDestination = `${destination}.next`;
    await cp(sourceArchive, stagedDestination);
    await rename(stagedDestination, destination);
    return { tarballSha256, memberCount: report.entryCount, destination };
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const sdkSource = process.env.TESLATLAS_SDK_SOURCE;
  const nodeExecutable = process.env.TESLATLAS_SDK_NODE;
  if (!sdkSource || !nodeExecutable) {
    throw new Error('TESLATLAS_SDK_SOURCE and TESLATLAS_SDK_NODE are required');
  }
  const result = await prepareSdkArtifact({
    projectRoot: resolve(import.meta.dirname, '..'),
    sdkSource,
    nodeExecutable,
  });
  process.stdout.write(
    `Prepared @teslatlas/sdk (${result.memberCount} files, ${result.tarballSha256}) at ${relative(resolve(import.meta.dirname, '..'), result.destination)}\n`,
  );
}
