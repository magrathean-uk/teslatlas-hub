import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const temporaryRoot = await mkdtemp(join(tmpdir(), "teslatlas-clean-package-"));
const npmExecutable = process.platform === "win32" ? "npm.cmd" : "npm";
const validatorNames = [
  "validateHubClaim",
  "validateHubCurrent",
  "validateHubDiscovery",
  "validateHubDrives",
  "validateHubError",
  "validateHubHealth",
  "validateHubInvitation",
  "validateHubReady",
  "validateHubVehicles",
];

function run(executable, args, cwd) {
  return execFileSync(executable, args, { cwd, encoding: "utf8", stdio: "pipe" });
}

try {
  const tarballs = [];
  for (const variant of ["clean", "stale"]) {
    const source = join(temporaryRoot, variant);
    await mkdir(source);
    // Deliberately omit every prior build output. Dependencies come from the caller's lock install.
    for (const path of [
      "src",
      "scripts",
      "docs",
      "LICENSE",
      "README.md",
      "package.json",
      "package-lock.json",
      ".npmrc",
      "tsconfig.json",
      "tsconfig.build.json",
    ]) {
      await cp(join(repositoryRoot, path), join(source, path), { recursive: true });
    }
    // Copy rather than symlink: bundler region comments include resolved dependency paths.
    await cp(join(repositoryRoot, "node_modules"), join(source, "node_modules"), {
      recursive: true,
      verbatimSymlinks: true,
    });
    if (variant === "stale") {
      await mkdir(join(source, "dist/generated"), { recursive: true });
      await writeFile(join(source, "dist/generated/hub-validators.d.ts"), "invalid stale types");
      await writeFile(join(source, "dist/obsolete.js"), "throw new Error('stale runtime');");
    }
    run(process.execPath, ["scripts/build.mjs"], source);
    const [report] = JSON.parse(run(npmExecutable, ["pack", "--json"], source));
    const tarball = join(source, report.filename);
    tarballs.push(await readFile(tarball));

    const consumer = join(temporaryRoot, `consumer-${variant}`);
    const installed = join(consumer, "node_modules/@teslatlas/sdk");
    await mkdir(installed, { recursive: true });
    run("tar", ["-xzf", tarball, "--strip-components=1", "-C", installed], consumer);
    await writeFile(join(consumer, "package.json"), '{"private":true,"type":"module"}\n');
    await cp(
      join(repositoryRoot, "tests/typecheck/public-package-api.ts"),
      join(consumer, "public-package-api.ts"),
    );
    await writeFile(
      join(consumer, "validators.ts"),
      `import type { HubClient } from "@teslatlas/sdk";
import { createHubClient as createNodeHubClient } from "@teslatlas/sdk/node";
import { createHubClient as createBrowserHubClient } from "@teslatlas/sdk/browser";
import { ${validatorNames.join(", ")} } from "./node_modules/@teslatlas/sdk/dist/generated/hub-validators.js";
declare const options: Parameters<typeof createNodeHubClient>[0];
const clients: HubClient[] = [createNodeHubClient(options), createBrowserHubClient(options)];
void clients;
for (const validate of [${validatorNames.join(", ")}]) {
  const valid: boolean = validate({});
  const errors: unknown = validate.errors;
  // @ts-expect-error Validators must return boolean, not any or string.
  const invalid: string = validate({});
  void [valid, errors, invalid];
}
`,
    );
    await writeFile(
      join(consumer, "tsconfig.json"),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          skipLibCheck: false,
          noEmit: true,
          target: "ES2022",
          module: "NodeNext",
          moduleResolution: "NodeNext",
          types: [],
        },
        include: ["*.ts", "node_modules/@teslatlas/sdk/dist/**/*.d.ts"],
      }),
    );
    run(
      process.execPath,
      [join(repositoryRoot, "node_modules/typescript/bin/tsc"), "-p", "tsconfig.json"],
      consumer,
    );
    assert.equal(
      await readFile(join(installed, "dist/generated/hub-validators.d.ts"), "utf8"),
      await readFile(join(source, "src/generated/hub-validators.d.ts"), "utf8"),
      "Packed generated declaration must match its source",
    );
    for (const dependency of ["ajv", "ajv-formats"]) {
      await symlink(
        join(repositoryRoot, "node_modules", dependency),
        join(consumer, "node_modules", dependency),
        "junction",
      );
    }
    const exports = JSON.parse(
      run(
        process.execPath,
        [
          "--input-type=module",
          "--eval",
          `const validators = await import(${JSON.stringify(`${installed}/dist/generated/hub-validators.js`)});
console.log(JSON.stringify(Object.keys(validators).sort()));`,
        ],
        consumer,
      ),
    );
    assert.deepEqual(exports, validatorNames, "Generated runtime and consumer names must agree");
    assert(!report.files.some((file) => file.path === "dist/obsolete.js"));
  }
  assert.deepEqual(tarballs[0], tarballs[1], "Stale dist must not change package bytes");
  console.log(
    `Clean and stale builds produced identical packs; packed declaration consumers passed. SHA-256: ${createHash("sha256").update(tarballs[0]).digest("hex")}`,
  );
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}
