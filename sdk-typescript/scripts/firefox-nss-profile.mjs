import { execFile } from "node:child_process";
import { access, chmod, lstat, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
export const FIREFOX_NSS_CERTUTIL = "/opt/homebrew/opt/nss/bin/certutil";
const certificateNickname = "Teslatlas Hub disposable CA";

export async function withFirefoxNssProfile(options, operation, dependencies = {}) {
  const certutilPath = options.certutilPath ?? FIREFOX_NSS_CERTUTIL;
  if (certutilPath !== FIREFOX_NSS_CERTUTIL) throw new Error("certutil path is not approved");
  if (typeof operation !== "function") throw new Error("Firefox profile operation is required");
  const execute = dependencies.execFile ?? execFileAsync;
  const createTemporary = dependencies.mkdtemp ?? mkdtemp;
  const remove = dependencies.rm ?? rm;
  const changeMode = dependencies.chmod ?? chmod;
  const checkAccess = dependencies.access ?? access;
  const requestedTemporaryParent = resolve(options.temporaryParent ?? tmpdir());
  const resolveRealPath = dependencies.realpath ?? realpath;
  const temporaryParent = await resolveRealPath(requestedTemporaryParent);

  await requireOwnerOnlyPath(options.certificatePath, "Hub CA", "file", dependencies);
  await requireOwnerOnlyPath(
    temporaryParent,
    "Firefox temporary parent",
    "directory",
    dependencies,
  );
  await checkAccess(certutilPath, 1);

  const profilePrefix = "teslatlas-firefox-nss-";
  const profilePath = await createTemporary(join(temporaryParent, profilePrefix));
  if (
    dirname(profilePath) !== temporaryParent ||
    !basename(profilePath).startsWith(profilePrefix)
  ) {
    throw new Error("Firefox profile escaped its private temporary parent");
  }
  let context;
  try {
    await changeMode(profilePath, 0o700);
    await requireOwnerOnlyPath(profilePath, "Firefox profile", "directory", dependencies);
    const database = `sql:${profilePath}`;
    await execute(certutilPath, ["-N", "-d", database, "--empty-password"], processOptions());
    await execute(
      certutilPath,
      ["-A", "-d", database, "-n", certificateNickname, "-t", "C,,", "-i", options.certificatePath],
      processOptions(),
    );
    await execute(
      certutilPath,
      ["-L", "-d", database, "-n", certificateNickname],
      processOptions(),
    );
    for (const name of ["cert9.db", "key4.db", "pkcs11.txt"]) {
      const databasePath = join(profilePath, name);
      await changeMode(databasePath, 0o600);
      await requireOwnerOnlyPath(databasePath, `Firefox NSS ${name}`, "file", dependencies);
    }

    context = await options.firefox.launchPersistentContext(profilePath, {
      firefoxUserPrefs: { "security.enterprise_roots.enabled": false },
      headless: options.headless ?? true,
      ignoreHTTPSErrors: false,
    });
    const value = await operation(context);
    return {
      evidence: {
        certificateNickname,
        database: "sql:NSS",
        emptyPassword: true,
        enterpriseRoots: false,
        ignoreHTTPSErrors: false,
        profileCleanup: "removed",
      },
      value,
    };
  } finally {
    try {
      await context?.close();
    } finally {
      await remove(profilePath, { force: true, recursive: true });
    }
  }
}

export async function requireOwnerOnlyPath(path, label, kind, dependencies = {}) {
  if (typeof path !== "string" || !isAbsolute(path) || resolve(path) !== path) {
    throw new Error(`${label} path must be absolute and normalized`);
  }
  const resolveRealPath = dependencies.realpath ?? realpath;
  const readStat = dependencies.lstat ?? lstat;
  const [canonical, stat] = await Promise.all([resolveRealPath(path), readStat(path)]);
  if (canonical !== path || stat.isSymbolicLink())
    throw new Error(`${label} path must be canonical`);
  if (kind === "file" ? !stat.isFile() : !stat.isDirectory()) {
    throw new Error(`${label} must be a ${kind}`);
  }
  if (typeof process.getuid === "function" && stat.uid !== process.getuid()) {
    throw new Error(`${label} must be owned by the current user`);
  }
  if ((stat.mode & 0o077) !== 0) throw new Error(`${label} must be owner-only`);
  return path;
}

export async function requireOwnerOnlyOutputPath(path, label, dependencies = {}) {
  if (typeof path !== "string" || !isAbsolute(path) || resolve(path) !== path) {
    throw new Error(`${label} path must be absolute and normalized`);
  }
  await requireOwnerOnlyPath(dirname(path), `${label} parent`, "directory", dependencies);
  const readStat = dependencies.lstat ?? lstat;
  try {
    await readStat(path);
  } catch (error) {
    if (error?.code === "ENOENT") return path;
    throw error;
  }
  throw new Error(`${label} must not already exist`);
}

function processOptions() {
  return {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    shell: false,
  };
}
