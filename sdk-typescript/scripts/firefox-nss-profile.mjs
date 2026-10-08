import { execFile } from "node:child_process";
import { access, chmod, lstat, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
export const FIREFOX_NSS_CERTUTIL = "/opt/homebrew/opt/nss/bin/certutil";
const certificateNickname = "Teslatlas Hub disposable CA";

// This error belongs to the private script boundary, not the public SDK surface.
export class FirefoxNssCleanupError extends AggregateError {
  constructor(errors, deferredCleanup) {
    super(errors, `Firefox profile cleanup deferred: ${deferredCleanup.reason}`, {
      cause: errors[0],
    });
    this.name = "FirefoxNssCleanupError";
    Object.defineProperty(this, "deferredCleanup", { value: deferredCleanup, enumerable: true });
  }
}

export async function withFirefoxNssProfile(options, operation, dependencies = {}) {
  const certutilPath = options.certutilPath ?? FIREFOX_NSS_CERTUTIL;
  if (certutilPath !== FIREFOX_NSS_CERTUTIL) throw new Error("certutil path is not approved");
  if (typeof operation !== "function") throw new Error("Firefox profile operation is required");
  const closeTimeoutMs = boundedTimeout(options.closeTimeoutMs ?? 30_000);
  const certutilTimeoutMs = boundedTimeout(options.certutilTimeoutMs ?? 30_000);
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
  let closeContext;
  let launchAttempted = false;
  let authority;
  let value;
  let primaryError;
  let failed = false;
  try {
    await changeMode(profilePath, 0o700);
    await requireOwnerOnlyPath(profilePath, "Firefox profile", "directory", dependencies);
    authority = await captureProfileAuthority(profilePath, temporaryParent, dependencies);
    const database = `sql:${profilePath}`;
    await execute(
      certutilPath,
      ["-N", "-d", database, "--empty-password"],
      processOptions(certutilTimeoutMs),
    );
    await execute(
      certutilPath,
      ["-A", "-d", database, "-n", certificateNickname, "-t", "C,,", "-i", options.certificatePath],
      processOptions(certutilTimeoutMs),
    );
    await execute(
      certutilPath,
      ["-L", "-d", database, "-n", certificateNickname],
      processOptions(certutilTimeoutMs),
    );
    for (const name of ["cert9.db", "key4.db", "pkcs11.txt"]) {
      const databasePath = join(profilePath, name);
      await changeMode(databasePath, 0o600);
      await requireOwnerOnlyPath(databasePath, `Firefox NSS ${name}`, "file", dependencies);
    }

    launchAttempted = true;
    context = await options.firefox.launchPersistentContext(profilePath, {
      firefoxUserPrefs: { "security.enterprise_roots.enabled": false },
      headless: options.headless ?? true,
      ignoreHTTPSErrors: false,
    });
    closeContext = context.close.bind(context);
    value = await operation(context);
  } catch (error) {
    failed = true;
    primaryError = error;
  }
  const cleanup = createProfileCleanup({
    profilePath,
    temporaryParent,
    authority,
    closeContext,
    launchAttempted,
    closeTimeoutMs,
    remove,
    dependencies,
    failed,
    primaryError,
  });
  await cleanup.retry();
  if (failed) throw primaryError;
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
}

function directoryIdentity(stat) {
  if (
    stat.dev === undefined ||
    stat.ino === undefined ||
    stat.uid === undefined ||
    !stat.isDirectory() ||
    stat.isSymbolicLink() ||
    (stat.mode & 0o077) !== 0 ||
    (typeof process.getuid === "function" && stat.uid !== process.getuid())
  ) {
    throw new Error("Firefox profile ownership cannot be established");
  }
  return Object.freeze({ dev: stat.dev, ino: stat.ino, uid: stat.uid });
}

function sameDirectory(stat, identity) {
  const current = directoryIdentity(stat);
  return (
    current.dev === identity.dev && current.ino === identity.ino && current.uid === identity.uid
  );
}

async function captureProfileAuthority(profilePath, parent, dependencies) {
  const readStat = dependencies.lstat ?? lstat;
  const resolvePath = dependencies.realpath ?? realpath;
  const [canonicalProfile, canonicalParent, profileStat, parentStat] = await Promise.all([
    resolvePath(profilePath),
    resolvePath(parent),
    readStat(profilePath),
    readStat(parent),
  ]);
  if (
    canonicalProfile !== profilePath ||
    canonicalParent !== parent ||
    dirname(profilePath) !== parent
  ) {
    throw new Error("Firefox profile ownership cannot be established");
  }
  return Object.freeze({
    profile: directoryIdentity(profileStat),
    parent: directoryIdentity(parentStat),
  });
}

function createProfileCleanup({
  profilePath,
  temporaryParent,
  authority,
  closeContext,
  launchAttempted,
  closeTimeoutMs,
  remove,
  dependencies,
  failed,
  primaryError,
}) {
  const readStat = dependencies.lstat ?? lstat;
  const resolvePath = dependencies.realpath ?? realpath;
  let closeState = "none";
  let closeError;
  let reason = "ownership_unknown";
  let removed = false;
  let tail = Promise.resolve();
  const waiters = new Set();

  const startClose = () => {
    closeState = "pending";
    // Observe each attempt once. Timeouts remove waiters rather than adding a
    // Promise.race reaction on the same indefinitely pending close every retry.
    Promise.resolve()
      .then(closeContext)
      .then(
        () => {
          closeState = "fulfilled";
          for (const notify of waiters) notify();
        },
        (error) => {
          closeState = "rejected";
          closeError = error;
          for (const notify of waiters) notify();
        },
      );
  };
  const awaitClose = () =>
    new Promise((resolve, reject) => {
      const finish = () => {
        if (closeState === "pending") return;
        clearTimeout(timer);
        waiters.delete(finish);
        if (closeState === "fulfilled") resolve();
        else {
          reason = "close_rejected";
          reject(closeError);
        }
      };
      const timer = setTimeout(() => {
        waiters.delete(finish);
        reason = "close_timed_out";
        reject(new Error("Firefox close timed out"));
      }, closeTimeoutMs);
      waiters.add(finish);
      finish();
    });

  const verifyOwnership = async () => {
    reason = "ownership_changed";
    if (authority === undefined) {
      reason = "ownership_unknown";
      throw new Error("Firefox profile ownership cannot be established");
    }
    const [canonicalParent, parentStat] = await Promise.all([
      resolvePath(temporaryParent),
      readStat(temporaryParent),
    ]);
    if (canonicalParent !== temporaryParent || !sameDirectory(parentStat, authority.parent)) {
      throw new Error("Firefox profile parent ownership changed");
    }
    let profileStat;
    try {
      profileStat = await readStat(profilePath);
    } catch (error) {
      if (error?.code === "ENOENT") return false;
      throw error;
    }
    if (
      !sameDirectory(profileStat, authority.profile) ||
      (await resolvePath(profilePath)) !== profilePath
    ) {
      throw new Error("Firefox profile ownership changed");
    }
    return true;
  };
  const attempt = async () => {
    if (removed) return;
    if (launchAttempted) {
      if (closeContext === undefined) {
        reason = "settlement_unknown";
        throw new Error("Firefox launch settlement is unknown");
      }
      if (closeState === "none" || closeState === "rejected") startClose();
      await awaitClose();
    }
    const exists = await verifyOwnership();
    if (exists) {
      reason = "remove_failed";
      await remove(profilePath, { force: true, recursive: true });
    }
    removed = true;
  };
  const record = Object.freeze({
    profilePath,
    get reason() {
      return reason;
    },
    get state() {
      return removed ? "removed" : "deferred";
    },
    retry() {
      const run = tail.then(attempt);
      tail = run.catch(() => undefined);
      return run.catch((error) => {
        throw new FirefoxNssCleanupError(failed ? [primaryError, error] : [error], record);
      });
    },
  });
  return record;
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

function boundedTimeout(value) {
  if (!Number.isSafeInteger(value) || value < 1 || value > 2_147_483_647) {
    throw new Error("Firefox timeout must be a positive bounded integer");
  }
  return value;
}

function processOptions(timeout) {
  return {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    shell: false,
    timeout,
    killSignal: "SIGKILL",
  };
}
