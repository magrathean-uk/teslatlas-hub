import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";
import standaloneCode from "ajv/dist/standalone/index.js";
import { _ } from "ajv/dist/compile/codegen/index.js";

const installedRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
if (process.argv.length !== 2) {
  throw new Error(
    "Usage: generate-protocol.mjs (TESLATLAS_PROTOCOL_OUTPUT_DIR selects a staging directory)",
  );
}
// The contract is read in place from the Protocol package of this repository. Nothing is vendored.
const sourceRoot = resolve(installedRoot, "../protocol/profiles/hub-http-v1/1.2.0");
const outputRoot = resolve(
  process.env.TESLATLAS_PROTOCOL_OUTPUT_DIR ?? join(installedRoot, "src/generated"),
);
const openapiExecutable = join(installedRoot, "node_modules/openapi-typescript/bin/cli.js");

const hubValidatorRefs = {
  validateHubDiscovery: "urn:teslatlas:hub-http-v1:1.2.0:discovery",
  validateHubError: "urn:teslatlas:hub-http-v1:1.2.0:errors",
  validateHubClaim: "urn:teslatlas:hub-http-v1:1.2.0:auth#/$defs/claim",
  validateHubInvitation: "urn:teslatlas:hub-http-v1:1.2.0:auth#/$defs/invitation",
  validateHubVehicles: "urn:teslatlas:hub-http-v1:1.2.0:resources#/$defs/vehicles",
  validateHubCurrent: "urn:teslatlas:hub-http-v1:1.2.0:resources#/$defs/current",
  validateHubDrives: "urn:teslatlas:hub-http-v1:1.2.0:resources#/$defs/drives",
  validateHubHealth: "urn:teslatlas:hub-http-v1:1.2.0:resources#/$defs/health",
  validateHubReady: "urn:teslatlas:hub-http-v1:1.2.0:resources#/$defs/ready",
};

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

// Augment in-memory embedded schema inputs only; the Protocol files stay intact.
function withExactIntegers(value) {
  if (Array.isArray(value)) return value.map(withExactIntegers);
  if (value === null || typeof value !== "object") return value;
  const output = Object.fromEntries(
    Object.entries(value).map(([key, child]) => [key, withExactIntegers(child)]),
  );
  if (value.type === "integer" || (Array.isArray(value.type) && value.type.includes("integer"))) {
    output.exactWireInteger = true;
  }
  return output;
}

function createAjv() {
  const ajv = new Ajv2020({
    allErrors: false,
    strictNumbers: true,
    code: { esm: true, source: true },
    strict: false,
  });
  ajv.addKeyword({
    keyword: "exactWireInteger",
    type: "number",
    schemaType: "boolean",
    code(context) {
      const { gen, data, it } = context;
      const check = gen.scopeValue("func", { ref: () => true, code: _`isExactJsonInteger` });
      context.fail(_`!${check}(${data}, ${it.parentData}, ${it.parentDataProperty})`);
    },
  });
  return ajv;
}

async function generateHubValidators() {
  const ajv = createAjv();
  addFormats(ajv);
  for (const file of [
    "auth.schema.json",
    "discovery.schema.json",
    "errors.schema.json",
    "resources.schema.json",
  ]) {
    ajv.addSchema(withExactIntegers(await readJson(join(sourceRoot, file))));
  }
  const standalone = standaloneCode(ajv, hubValidatorRefs)
    .replaceAll('require("ajv-formats/dist/formats").fullFormats', "ajvFormats.fullFormats")
    .replaceAll('require("ajv/dist/runtime/equal").default', "ajvEqualRuntime")
    .replaceAll('require("ajv/dist/runtime/ucs2length").default', "ajvUcs2LengthRuntime");
  if (standalone.includes("require(")) {
    throw new Error("Generated Hub validators must not contain CommonJS runtime helpers");
  }
  return `// @generated
import { isExactJsonInteger } from "../http/bounded-json.js";
import ajvFormats from "ajv-formats/dist/formats.js";
import ajvEqual from "ajv/dist/runtime/equal.js";
import ajvUcs2Length from "ajv/dist/runtime/ucs2length.js";
const ajvEqualRuntime = typeof ajvEqual === "function" ? ajvEqual : ajvEqual.default;
const ajvUcs2LengthRuntime = typeof ajvUcs2Length === "function" ? ajvUcs2Length : ajvUcs2Length.default;
${standalone}`;
}

await mkdir(outputRoot, { recursive: true });
const hubProtocolOutput = join(outputRoot, "hub-protocol.ts");
execFileSync(
  process.execPath,
  [openapiExecutable, join(sourceRoot, "openapi.json"), "--output", hubProtocolOutput],
  { cwd: installedRoot, stdio: "inherit" },
);
await writeFile(
  hubProtocolOutput,
  `// @ts-nocheck\n// @generated\n${await readFile(hubProtocolOutput, "utf8")}`,
);
await writeFile(join(outputRoot, "hub-validators.js"), await generateHubValidators());
await writeFile(
  join(outputRoot, "hub-validators.d.ts"),
  `// @generated
export interface GeneratedHubValidator {
  (value: unknown): boolean;
  readonly errors?: unknown;
}
${Object.keys(hubValidatorRefs)
  .sort()
  .map((name) => `export const ${name}: GeneratedHubValidator;`)
  .join("\n")}
`,
);
