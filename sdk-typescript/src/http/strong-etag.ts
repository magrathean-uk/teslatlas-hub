import { containsControlCharacters, TeslatlasError } from "../core/errors.js";

const strongEntityTagBrand: unique symbol = Symbol("StrongEntityTag");
const maximumStrongEntityTagLength = 512;
const strongEntityTagPattern = /^"[^"]+"$/u;

export type StrongEntityTag = string & {
  readonly [strongEntityTagBrand]: true;
};

export class InvalidStrongEntityTagError extends TeslatlasError<"invalid_strong_entity_tag"> {
  constructor() {
    super("Strong entity tag must use the protocol ETag syntax", {
      code: "invalid_strong_entity_tag",
    });
  }
}

export function asStrongEntityTag(value: string): StrongEntityTag {
  if (
    typeof value !== "string" ||
    value.length > maximumStrongEntityTagLength ||
    !isStrongEntityTag(value)
  ) {
    throw new InvalidStrongEntityTagError();
  }
  return value as StrongEntityTag;
}

export function isStrongEntityTag(value: unknown): value is string {
  return (
    typeof value === "string" &&
    !containsControlCharacters(value) &&
    strongEntityTagPattern.test(value)
  );
}
