import { access, chmod, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  FIREFOX_NSS_CERTUTIL,
  requireOwnerOnlyOutputPath,
  requireOwnerOnlyPath,
  withFirefoxNssProfile,
} from "../../scripts/firefox-nss-profile.mjs";

const roots: string[] = [];
afterEach(async () =>
  Promise.all(roots.splice(0).map((root) => rm(root, { force: true, recursive: true }))),
);

describe("disposable Firefox NSS profile", () => {
  it("initializes SQL NSS, imports only the Hub CA, launches strict Firefox, and cleans up", async () => {
    const root = await ownerOnlyRoot();
    const certificatePath = join(root, "hub-ca.pem");
    await writeFile(certificatePath, "test Hub CA", { mode: 0o600 });
    const commands: string[][] = [];
    const executables: string[] = [];
    const execute = vi.fn(async (executable: string, arguments_: string[]) => {
      executables.push(executable);
      commands.push(arguments_);
      if (arguments_[0] === "-N") {
        const profile = arguments_[2]?.replace(/^sql:/u, "");
        if (profile === undefined) throw new Error("missing fake NSS profile");
        for (const name of ["cert9.db", "key4.db", "pkcs11.txt"]) {
          await writeFile(join(profile, name), name, { mode: 0o600 });
        }
      }
      return { stderr: "", stdout: "" };
    });
    const close = vi.fn(async () => undefined);
    let launchedProfile = "";
    const launchPersistentContext = vi.fn(async (profile: string) => {
      launchedProfile = profile;
      return { close };
    });

    const result = await withFirefoxNssProfile(
      {
        certificatePath,
        firefox: { launchPersistentContext },
        temporaryParent: root,
      },
      async () => "route-passed",
      { access: async () => undefined, execFile: execute },
    );

    expect(commands).toEqual([
      ["-N", "-d", `sql:${launchedProfile}`, "--empty-password"],
      [
        "-A",
        "-d",
        `sql:${launchedProfile}`,
        "-n",
        "Teslatlas Hub disposable CA",
        "-t",
        "C,,",
        "-i",
        certificatePath,
      ],
      ["-L", "-d", `sql:${launchedProfile}`, "-n", "Teslatlas Hub disposable CA"],
    ]);
    expect(executables).toEqual(Array.from({ length: 3 }, () => FIREFOX_NSS_CERTUTIL));
    expect(execute).toHaveBeenCalledTimes(3);
    expect(launchPersistentContext).toHaveBeenCalledWith(launchedProfile, {
      firefoxUserPrefs: { "security.enterprise_roots.enabled": false },
      headless: true,
      ignoreHTTPSErrors: false,
    });
    expect(result).toMatchObject({
      value: "route-passed",
      evidence: {
        database: "sql:NSS",
        emptyPassword: true,
        enterpriseRoots: false,
        ignoreHTTPSErrors: false,
        profileCleanup: "removed",
      },
    });
    expect(close).toHaveBeenCalledOnce();
    await expect(access(launchedProfile)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects non-owner-only and symlinked certificate inputs before certutil", async () => {
    const root = await ownerOnlyRoot();
    const openCertificate = join(root, "open.pem");
    const certificate = join(root, "certificate.pem");
    const linkedCertificate = join(root, "linked.pem");
    await writeFile(openCertificate, "open", { mode: 0o644 });
    await writeFile(certificate, "private", { mode: 0o600 });
    await symlink(certificate, linkedCertificate);

    await expect(requireOwnerOnlyPath(openCertificate, "Hub CA", "file")).rejects.toThrow(
      "owner-only",
    );
    await expect(requireOwnerOnlyPath(linkedCertificate, "Hub CA", "file")).rejects.toThrow(
      "canonical",
    );
    await expect(requireOwnerOnlyOutputPath(certificate, "receipt")).rejects.toThrow(
      "must not already exist",
    );
  });

  it("closes Firefox and removes the exact profile when the route fails", async () => {
    const root = await ownerOnlyRoot();
    const certificatePath = join(root, "hub-ca.pem");
    await writeFile(certificatePath, "test Hub CA", { mode: 0o600 });
    const close = vi.fn(async () => undefined);
    let launchedProfile = "";

    await expect(
      withFirefoxNssProfile(
        {
          certificatePath,
          firefox: {
            launchPersistentContext: async (profile: string) => {
              launchedProfile = profile;
              return { close };
            },
          },
          temporaryParent: root,
        },
        async () => {
          throw new Error("route failed");
        },
        {
          access: async () => undefined,
          execFile: async (_executable: string, arguments_: string[]) => {
            if (arguments_[0] === "-N") {
              const profile = arguments_[2]?.replace(/^sql:/u, "");
              if (profile === undefined) throw new Error("missing fake NSS profile");
              for (const name of ["cert9.db", "key4.db", "pkcs11.txt"]) {
                await writeFile(join(profile, name), name, { mode: 0o600 });
              }
            }
            return { stderr: "", stdout: "" };
          },
        },
      ),
    ).rejects.toThrow("route failed");
    expect(close).toHaveBeenCalledOnce();
    await expect(access(launchedProfile)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("still removes the profile if Firefox close fails", async () => {
    const root = await ownerOnlyRoot();
    const certificatePath = join(root, "hub-ca.pem");
    await writeFile(certificatePath, "test Hub CA", { mode: 0o600 });
    let launchedProfile = "";

    await expect(
      withFirefoxNssProfile(
        {
          certificatePath,
          firefox: {
            launchPersistentContext: async (profile: string) => {
              launchedProfile = profile;
              return { close: async () => Promise.reject(new Error("close failed")) };
            },
          },
          temporaryParent: root,
        },
        async () => "route-passed",
        {
          access: async () => undefined,
          execFile: fakeCertutil,
        },
      ),
    ).rejects.toThrow("close failed");
    await expect(access(launchedProfile)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects a temporary profile outside the approved parent before recursive cleanup", async () => {
    const root = await ownerOnlyRoot();
    const otherRoot = await ownerOnlyRoot();
    const certificatePath = join(root, "hub-ca.pem");
    await writeFile(certificatePath, "test Hub CA", { mode: 0o600 });
    const remove = vi.fn(async () => undefined);

    await expect(
      withFirefoxNssProfile(
        {
          certificatePath,
          firefox: { launchPersistentContext: vi.fn() },
          temporaryParent: root,
        },
        async () => undefined,
        {
          access: async () => undefined,
          mkdtemp: async () => otherRoot,
          remove,
        },
      ),
    ).rejects.toThrow("escaped its private temporary parent");
    expect(remove).not.toHaveBeenCalled();
  });

  it("rejects any certutil executable outside the approved Homebrew NSS path", async () => {
    const root = await ownerOnlyRoot();
    const certificatePath = join(root, "hub-ca.pem");
    await writeFile(certificatePath, "test Hub CA", { mode: 0o600 });

    await expect(
      withFirefoxNssProfile(
        {
          certificatePath,
          certutilPath: join(root, "certutil"),
          firefox: { launchPersistentContext: vi.fn() },
          temporaryParent: root,
        },
        async () => undefined,
      ),
    ).rejects.toThrow("certutil path is not approved");
  });
});

async function fakeCertutil(_executable: string, arguments_: string[]) {
  if (arguments_[0] === "-N") {
    const profile = arguments_[2]?.replace(/^sql:/u, "");
    if (profile === undefined) throw new Error("missing fake NSS profile");
    for (const name of ["cert9.db", "key4.db", "pkcs11.txt"]) {
      await writeFile(join(profile, name), name, { mode: 0o600 });
    }
  }
  return { stderr: "", stdout: "" };
}

async function ownerOnlyRoot() {
  const root = await realpath(await mkdtemp(join(tmpdir(), "teslatlas-firefox-nss-test-")));
  await chmod(root, 0o700);
  roots.push(root);
  return root;
}
