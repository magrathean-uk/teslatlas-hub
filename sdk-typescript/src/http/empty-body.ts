import { ProtocolValidationError } from "../core/errors.js";

const maximumEmptyChunks = 1_024;

export async function requireEmptyResponseBody(
  response: Response,
  signal: AbortSignal | undefined,
  validatorName: string,
): Promise<void> {
  if (response.body === null) return;
  const reader = response.body.getReader();
  let abortListener: (() => void) | undefined;
  const aborted =
    signal === undefined
      ? undefined
      : new Promise<never>((_resolve, reject) => {
          abortListener = () => reject(signal.reason ?? new DOMException("Aborted", "AbortError"));
          signal.addEventListener("abort", abortListener, { once: true });
        });
  try {
    if (signal?.aborted === true) throw signal.reason ?? new DOMException("Aborted", "AbortError");
    for (let count = 0; count < maximumEmptyChunks; count += 1) {
      const result = await (aborted === undefined
        ? reader.read()
        : Promise.race([reader.read(), aborted]));
      if (result.done) return;
      if (result.value.byteLength !== 0) throw new ProtocolValidationError(validatorName);
    }
    throw new ProtocolValidationError(validatorName);
  } catch (error) {
    if (signal?.aborted === true) throw signal.reason ?? error;
    if (error instanceof ProtocolValidationError) throw error;
    throw new ProtocolValidationError(validatorName);
  } finally {
    if (abortListener !== undefined) signal?.removeEventListener("abort", abortListener);
    void reader.cancel().catch(() => undefined);
  }
}
