import { Ajv2020, type Schema } from "ajv/dist/2020.js";
import { ProtocolValidationError } from "../core/errors.js";
import { requireCapability } from "../protocol/capabilities.js";
import type { CommandRequest, HubDescriptor } from "../protocol/models.js";

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
  const synchronousSchema = asSynchronousSchema(schema);
  if (synchronousSchema === undefined) return false;
  try {
    return new Ajv2020({ allErrors: false, strict: false }).compile(synchronousSchema)(value);
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
