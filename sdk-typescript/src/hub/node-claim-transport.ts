import { createHash } from "node:crypto";
import { request as httpsRequest } from "node:https";
import { checkServerIdentity } from "node:tls";
import { ProtocolValidationError, TransportError } from "../core/errors.js";
import { HubTlsPinMismatchError, HubTlsPinUnavailableError } from "./client.js";
import type { HubClaimTransport } from "./models.js";

const maximumResponseBytes = 1_048_576;

export interface NodeHubTlsOptions {
  /** CA certificates for the claim connection. Omit to use Node's configured trust roots. */
  readonly ca?: string | Uint8Array | readonly (string | Uint8Array)[];
}

export function createNodeHubClaimTransport(options: NodeHubTlsOptions = {}): HubClaimTransport {
  const ca = normalizeCa(options.ca);
  return ({ url, body, tlsPin, signal }) => {
    if (url.protocol !== "https:") return Promise.reject(new HubTlsPinUnavailableError());
    return new Promise<Response>((resolve, reject) => {
      let settled = false;
      const settle = (operation: () => void) => {
        if (settled) return;
        settled = true;
        operation();
      };
      const request = httpsRequest(
        url,
        {
          agent: false,
          ca,
          method: "POST",
          rejectUnauthorized: true,
          signal,
          headers: {
            "Content-Length": Buffer.byteLength(body),
            "Content-Type": "application/json",
          },
          checkServerIdentity: (hostname, certificate) => {
            const identityError = checkServerIdentity(hostname, certificate);
            if (identityError !== undefined) return identityError;
            const actualPin = createHash("sha256").update(certificate.raw).digest("hex");
            return actualPin === tlsPin ? undefined : new HubTlsPinMismatchError();
          },
        },
        (incoming) => {
          const chunks: Buffer[] = [];
          let size = 0;
          incoming.on("data", (chunk: Buffer) => {
            size += chunk.byteLength;
            if (size > maximumResponseBytes) {
              incoming.destroy();
              settle(() => reject(new ProtocolValidationError("HubResponse.size")));
              return;
            }
            chunks.push(chunk);
          });
          incoming.on("error", () => settle(() => reject(new TransportError())));
          incoming.on("end", () => {
            try {
              const status = incoming.statusCode;
              if (status === undefined || status < 200 || status > 599) {
                throw new ProtocolValidationError("HubResponse.status");
              }
              const headers = new Headers();
              for (let index = 0; index < incoming.rawHeaders.length; index += 2) {
                const name = incoming.rawHeaders[index];
                const value = incoming.rawHeaders[index + 1];
                if (name !== undefined && value !== undefined) headers.append(name, value);
              }
              const response = new Response(
                status === 204 || status === 205 || status === 304 ? null : Buffer.concat(chunks),
                {
                  status,
                  ...(incoming.statusMessage === undefined
                    ? {}
                    : { statusText: incoming.statusMessage }),
                  headers,
                },
              );
              settle(() => resolve(response));
            } catch (error) {
              settle(() =>
                reject(
                  error instanceof ProtocolValidationError
                    ? error
                    : new ProtocolValidationError("HubResponse"),
                ),
              );
            }
          });
        },
      );
      request.on("error", (error) => {
        settle(() => {
          if (error instanceof HubTlsPinMismatchError) reject(error);
          else if (signal.aborted) reject(signal.reason ?? error);
          else reject(new TransportError());
        });
      });
      request.end(body);
    });
  };
}

function normalizeCa(
  value: NodeHubTlsOptions["ca"],
): string | Buffer | (string | Buffer)[] | undefined {
  if (value === undefined || typeof value === "string") return value;
  if (value instanceof Uint8Array) return Buffer.from(value);
  return value.map((entry) => (typeof entry === "string" ? entry : Buffer.from(entry)));
}
