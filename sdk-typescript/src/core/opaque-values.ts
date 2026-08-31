import { containsControlCharacters, TeslatlasError } from "./errors.js";

const opaqueCursorBrand: unique symbol = Symbol("OpaqueCursor");
const entityTagBrand: unique symbol = Symbol("EntityTag");
const maximumEntityTagLength = 512;
const entityTagPattern = /^(?:W\/)?"[^"]+"$/u;

export type OpaqueCursor = string & {
  readonly [opaqueCursorBrand]: true;
};

export type EntityTag = string & {
  readonly [entityTagBrand]: true;
};

export class InvalidOpaqueCursorError extends TeslatlasError<"invalid_opaque_cursor"> {
  constructor() {
    super("Opaque cursor must be non-empty and contain no control characters", {
      code: "invalid_opaque_cursor",
    });
  }
}

export class InvalidEntityTagError extends TeslatlasError<"invalid_entity_tag"> {
  constructor() {
    super("Entity tag must use the protocol ETag syntax", {
      code: "invalid_entity_tag",
    });
  }
}

export function asOpaqueCursor(value: string): OpaqueCursor {
  if (value.length === 0 || containsControlCharacters(value)) {
    throw new InvalidOpaqueCursorError();
  }
  return value as OpaqueCursor;
}

export function asEntityTag(value: string): EntityTag {
  if (
    value.length > maximumEntityTagLength ||
    containsControlCharacters(value) ||
    !entityTagPattern.test(value)
  ) {
    throw new InvalidEntityTagError();
  }
  return value as EntityTag;
}
