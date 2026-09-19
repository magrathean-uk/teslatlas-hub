import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";

const expectedSha256 = process.argv[2];
if (!/^[0-9a-f]{64}$/u.test(expectedSha256 ?? "")) {
  throw new Error("exact SDK archive SHA-256 is required");
}

const manifest = JSON.parse(
  await readFile(new URL("./node_modules/@teslatlas/sdk/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@teslatlas/sdk" ||
  manifest.version !== "2026.36.2" ||
  manifest.packageManager !== "npm@11.19.0" ||
  manifest.teslatlasSupport?.policy !== "evidence-only" ||
  manifest.teslatlasSupport?.reproducibilityToolchain?.node !== "26.7.0" ||
  manifest.teslatlasSupport?.reproducibilityToolchain?.npm !== "11.19.0" ||
  Object.values(manifest.teslatlasSupport?.declaredFloors ?? {}).some((value) => value !== null)
) {
  throw new Error("installed SDK metadata is not the reviewed candidate");
}

const root = await import("@teslatlas/sdk");
const node = await import("@teslatlas/sdk/node");
const browser = await import("@teslatlas/sdk/browser");
if (
  typeof root.TeslatlasError !== "function" ||
  typeof node.createHubClient !== "function" ||
  typeof browser.createHubClient !== "function"
) {
  throw new Error("installed SDK public surface is incomplete");
}

process.stdout.write(
  `${JSON.stringify({
    package: `${manifest.name}@${manifest.version}`,
    archiveSha256: expectedSha256,
    metadataSha256: createHash("sha256").update(JSON.stringify(manifest)).digest("hex"),
  })}\n`,
);
