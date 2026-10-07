import { Ajv2020, type Schema, type ValidateFunction } from "ajv/dist/2020.js";
import { ProtocolValidationError } from "../core/errors.js";
import { requireCapability } from "../protocol/capabilities.js";
import type { CommandRequest, HubDescriptor } from "../protocol/models.js";
import { snapshotJsonRequest } from "../http/json-request-snapshot.js";

interface CachedSchema {
  readonly body: string;
  readonly validate: ValidateFunction;
}

// One current compilation per live schema object. Replacing a descriptor does
// not retain its schemas, and nested mutation replaces rather than grows an entry.
const schemaValidators = new WeakMap<object, CachedSchema>();
const booleanValidators = new Map<boolean, CachedSchema>();

export function validateAdvertisedCommand(
  descriptor: HubDescriptor,
  command: CommandRequest,
): void {
  const capability = requireCapability(descriptor, "commands.async");
  const commandDescriptor = capability.commands?.find(({ name }) => name === command.command);
  if (
    commandDescriptor === undefined ||
    commandDescriptor.command_class !== command.command_class ||
    (commandDescriptor.confirmation_required && command.confirmation === undefined)
  ) {
    throw new ProtocolValidationError("command.descriptor");
  }
  if (
    !matchesJsonSchema(commandDescriptor.parameters_schema, command.parameters) ||
    !matchesJsonSchema(commandDescriptor.expected_state_schema, command.expected_state)
  ) {
    throw new ProtocolValidationError("command.descriptor");
  }
}

function matchesJsonSchema(schema: unknown, value: unknown): boolean {
  try {
    const snapshot = snapshotJsonRequest(schema);
    const synchronousSchema = asSynchronousSchema(snapshot.value);
    if (synchronousSchema === undefined) return false;
    const cached =
      typeof schema === "boolean"
        ? booleanValidators.get(schema)
        : schema !== null && typeof schema === "object"
          ? schemaValidators.get(schema)
          : undefined;
    if (cached?.body === snapshot.body) return cached.validate(value) === true;
    const validate = new Ajv2020({ allErrors: false, strict: false }).compile(synchronousSchema);
    const entry = { body: snapshot.body, validate };
    if (typeof schema === "boolean") booleanValidators.set(schema, entry);
    else if (schema !== null && typeof schema === "object") schemaValidators.set(schema, entry);
    return validate(value) === true;
  } catch {
    return false;
  }
}

function asSynchronousSchema(value: unknown): Schema | undefined {
  if (value === true || value === false) return value;
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    (value as { $async?: unknown }).$async === true
  ) {
    return undefined;
  }
  return value as Schema;
}
