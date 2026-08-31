import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";
import standaloneCode from "ajv/dist/standalone/index.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = join(repositoryRoot, "protocol/source");
const outputRoot = resolve(
  process.env.TESLATLAS_PROTOCOL_OUTPUT_DIR ?? join(repositoryRoot, "src/generated"),
);
const npmExecutable = process.platform === "win32" ? "npm.cmd" : "npm";

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

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function generateValidators() {
  const schemaDirectory = join(sourceRoot, "schemas");
  const schemaFiles = (await (await import("node:fs/promises")).readdir(schemaDirectory))
    .filter((file) => file.endsWith(".schema.json"))
    .sort();
  const ajv = new Ajv2020({ allErrors: true, code: { esm: true, source: true }, strict: false });
  addFormats(ajv);
  for (const file of schemaFiles) ajv.addSchema(await readJson(join(schemaDirectory, file)));
  return `// @ts-nocheck\n// @generated\n${standaloneCode(ajv, validatorRefs)}`;
}

function collectExamplePaths(value, paths = new Set()) {
  if (Array.isArray(value)) {
    value.forEach((child) => {
      collectExamplePaths(child, paths);
    });
  } else if (value !== null && typeof value === "object")
    Object.values(value).forEach((child) => {
      collectExamplePaths(child, paths);
    });
  else if (typeof value === "string" && /^examples\/.+\.json$/.test(value)) paths.add(value);
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
  const examplePaths = [...collectExamplePaths(cases)].sort();
  const documents = [
    ...(await Promise.all(compatibilityPaths.map((path) => readJson(join(sourceRoot, path))))),
    ...cases,
    ...(await Promise.all(examplePaths.map((path) => readJson(join(sourceRoot, path))))),
  ];
  return `// @generated\nexport const protocolCases: readonly unknown[] = Object.freeze(${JSON.stringify(documents, null, 2)});\n`;
}

await mkdir(outputRoot, { recursive: true });
const protocolOutput = join(outputRoot, "protocol.ts");
execFileSync(
  npmExecutable,
  [
    "exec",
    "--offline",
    "--",
    "openapi-typescript",
    "protocol/source/openapi/teslatlas-v1.openapi.json",
    "--output",
    protocolOutput,
  ],
  { cwd: repositoryRoot, stdio: "inherit" },
);
await writeFile(
  protocolOutput,
  `// @ts-nocheck\n// @generated\n${await readFile(protocolOutput, "utf8")}`,
);
await writeFile(join(outputRoot, "validators.ts"), await generateValidators());
await writeFile(join(outputRoot, "protocol-cases.ts"), await generateCases());
