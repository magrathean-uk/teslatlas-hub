import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, isAbsolute, join, resolve } from "node:path";
import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";
import standaloneCode from "ajv/dist/standalone/index.js";
import { _ } from "ajv/dist/compile/codegen/index.js";

const installedRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
if (
  args.length !== 0 &&
  (args.length !== 2 || args[0] !== "--output-root" || !isAbsolute(args[1]))
) {
  throw new Error("Usage: generate-protocol.mjs [--output-root ABSOLUTE_STAGED_ROOT]");
}
const repositoryRoot = args.length === 0 ? installedRoot : args[1];
const sourceRoot = join(repositoryRoot, "protocol/source");
const outputRoot = resolve(
  args.length === 0
    ? (process.env.TESLATLAS_PROTOCOL_OUTPUT_DIR ?? join(repositoryRoot, "src/generated"))
    : join(repositoryRoot, "src/generated"),
);
const openapiExecutable = join(installedRoot, "node_modules/openapi-typescript/bin/cli.js");

const validatorRefs = {
  validateDiscovery: "urn:teslatlas:protocol:schema:discovery:1.2.0",
  validateProblem: "urn:teslatlas:protocol:schema:error:1.2.0",
  validateVehiclePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/vehicle_page",
  validateCurrentState: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/current_state",
  validateDrivePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/drive_page",
  validateDrive: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/drive",
  validatePositionPage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/position_page",
  validateChargePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/charge_page",
  validateCharge: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/charge",
  validateChargeSamplePage:
    "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/charge_sample_page",
  validateStatePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/state_page",
  validateUpdatePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/update_page",
  validateDataQualityPage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/data_quality_page",
  validateCommandRequest: "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_request",
  validateCommandJob: "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_job",
  validateMetadataPage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/metadata_page",
  validateMetadataCreate: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_create",
  validateMetadataReplace: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_replace",
  validateMetadataRecord: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_record",
  validateMetadataTombstone:
    "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_tombstone",
  validateEvent: "urn:teslatlas:protocol:schema:event:1.2.0",
};

const hubValidatorRefs = {
  validateHubDiscovery: "urn:teslatlas:hub-http-v1:1.0.0:discovery",
  validateHubError: "urn:teslatlas:hub-http-v1:1.0.0:errors",
  validateHubClaim: "urn:teslatlas:hub-http-v1:1.0.0:auth#/$defs/claim",
  validateHubInvitation: "urn:teslatlas:hub-http-v1:1.0.0:auth#/$defs/invitation",
  validateHubVehicles: "urn:teslatlas:hub-http-v1:1.0.0:resources#/$defs/vehicles",
  validateHubCurrent: "urn:teslatlas:hub-http-v1:1.0.0:resources#/$defs/current",
  validateHubDrives: "urn:teslatlas:hub-http-v1:1.0.0:resources#/$defs/drives",
  validateHubHealth: "urn:teslatlas:hub-http-v1:1.0.0:resources#/$defs/health",
  validateHubReady: "urn:teslatlas:hub-http-v1:1.0.0:resources#/$defs/ready",
};

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

// Augment in-memory embedded schema inputs only; frozen Protocol files stay intact.
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

async function generateValidators() {
  const schemaDirectory = join(sourceRoot, "schemas");
  const schemaFiles = (await (await import("node:fs/promises")).readdir(schemaDirectory))
    .filter((file) => file.endsWith(".schema.json"))
    .sort();
  const ajv = createAjv();
  addFormats(ajv);
  for (const file of schemaFiles)
    ajv.addSchema(withExactIntegers(await readJson(join(schemaDirectory, file))));
  const standalone = standaloneCode(ajv, validatorRefs)
    .replaceAll('require("ajv-formats/dist/formats").fullFormats', "ajvFormats.fullFormats")
    .replaceAll('require("ajv/dist/runtime/equal").default', "ajvEqualRuntime")
    .replaceAll('require("ajv/dist/runtime/ucs2length").default', "ajvUcs2LengthRuntime");
  if (standalone.includes("require(")) {
    throw new Error("Generated validators must not contain CommonJS runtime helpers");
  }
  return `// @ts-nocheck
// @generated
import { isExactJsonInteger } from "../http/bounded-json.js";
import ajvFormats from "ajv-formats/dist/formats.js";
import ajvEqual from "ajv/dist/runtime/equal.js";
import ajvUcs2Length from "ajv/dist/runtime/ucs2length.js";
const ajvEqualRuntime = typeof ajvEqual === "function" ? ajvEqual : ajvEqual.default;
const ajvUcs2LengthRuntime = typeof ajvUcs2Length === "function" ? ajvUcs2Length : ajvUcs2Length.default;
${standalone}`;
}

async function generateHubValidators() {
  const profileDirectory = join(sourceRoot, "profiles/hub-http-v1/1.0.0");
  const ajv = createAjv();
  addFormats(ajv);
  for (const file of [
    "auth.schema.json",
    "discovery.schema.json",
    "errors.schema.json",
    "resources.schema.json",
  ]) {
    ajv.addSchema(withExactIntegers(await readJson(join(profileDirectory, file))));
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

const bodyFilePattern =
  /^(?:examples|fixtures)\/(?:[A-Za-z0-9][A-Za-z0-9._-]*\/)*[A-Za-z0-9][A-Za-z0-9._-]*$/;

function collectBodyPaths(value, paths = new Set()) {
  if (Array.isArray(value)) {
    value.forEach((child) => {
      collectBodyPaths(child, paths);
    });
  } else if (value !== null && typeof value === "object") {
    if (Object.hasOwn(value, "body_file")) {
      const path = value.body_file;
      if (typeof path !== "string" || !bodyFilePattern.test(path)) {
        throw new Error("Protocol case body path is invalid");
      }
      paths.add(path);
    }
    Object.values(value).forEach((child) => {
      collectBodyPaths(child, paths);
    });
  }
  return paths;
}

async function generateCases() {
  const caseDirectory = join(sourceRoot, "conformance/cases");
  const compatibilityPaths = [
    "compatibility/manifest.json",
    "compatibility/1.0.0/profile.json",
    "compatibility/1.1.0/profile.json",
    "compatibility/1.2.0/profile.json",
  ];
  const caseFiles = (await (await import("node:fs/promises")).readdir(caseDirectory))
    .filter((file) => file.endsWith(".json"))
    .sort();
  const cases = await Promise.all(caseFiles.map((file) => readJson(join(caseDirectory, file))));
  const bodyPaths = [...collectBodyPaths(cases)].sort();
  const bodies = Object.fromEntries(
    await Promise.all(
      bodyPaths.map(async (path) => [path, await readJson(join(sourceRoot, path))]),
    ),
  );
  const documents = [
    ...(await Promise.all(compatibilityPaths.map((path) => readJson(join(sourceRoot, path))))),
    ...cases,
  ];
  return `// @generated\nexport const protocolCases: readonly unknown[] = Object.freeze(${JSON.stringify(documents, null, 2)});\nexport const protocolCaseBodies: Readonly<Record<string, unknown>> = Object.freeze(${JSON.stringify(bodies, null, 2)});\n`;
}

async function generateEventCatalog() {
  const catalog = eventCatalogFromContract(
    await readJson(join(sourceRoot, "events/teslatlas-v1.sse.json")),
  );
  return `// @generated\nexport const protocolEventCatalog: readonly unknown[] = Object.freeze(${JSON.stringify(catalog, null, 2)});\n`;
}

function eventCatalogFromContract(contract) {
  if (contract === null || typeof contract !== "object" || !Array.isArray(contract.events)) {
    throw new Error("Protocol event contract is invalid");
  }
  const names = new Set();
  return contract.events.map((entry) => {
    if (
      entry === null ||
      typeof entry !== "object" ||
      typeof entry.name !== "string" ||
      entry.name.length === 0 ||
      typeof entry.introduced_in !== "string" ||
      entry.introduced_in.length === 0 ||
      names.has(entry.name)
    ) {
      throw new Error("Protocol event contract is invalid");
    }
    names.add(entry.name);
    return { name: entry.name, introduced_in: entry.introduced_in };
  });
}

await mkdir(outputRoot, { recursive: true });
const protocolOutput = join(outputRoot, "protocol.ts");
execFileSync(
  process.execPath,
  [
    openapiExecutable,
    join(sourceRoot, "openapi/teslatlas-v1.openapi.json"),
    "--output",
    protocolOutput,
  ],
  { cwd: installedRoot, stdio: "inherit" },
);
await writeFile(
  protocolOutput,
  `// @ts-nocheck\n// @generated\n${await readFile(protocolOutput, "utf8")}`,
);
await writeFile(join(outputRoot, "validators.ts"), await generateValidators());
await writeFile(join(outputRoot, "protocol-cases.ts"), await generateCases());
await writeFile(join(outputRoot, "event-catalog.ts"), await generateEventCatalog());

const hubProtocolOutput = join(outputRoot, "hub-protocol.ts");
execFileSync(
  process.execPath,
  [
    openapiExecutable,
    join(sourceRoot, "profiles/hub-http-v1/1.0.0/openapi.json"),
    "--output",
    hubProtocolOutput,
  ],
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
