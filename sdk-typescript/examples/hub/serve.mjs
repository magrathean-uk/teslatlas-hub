import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const directory = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const browserBundle = require.resolve("@teslatlas/sdk/browser");
const assets = new Map([
  ["/", { path: resolve(directory, "index.html"), type: "text/html; charset=utf-8" }],
  ["/app.js", { path: resolve(directory, "app.js"), type: "text/javascript; charset=utf-8" }],
  ["/sdk.js", { path: browserBundle, type: "text/javascript; charset=utf-8" }],
]);
const port = Number.parseInt(process.env.TESLATLAS_HUB_BROWSER_PORT ?? "4174", 10);
if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) {
  throw new Error("TESLATLAS_HUB_BROWSER_PORT must be an integer from 0 to 65535");
}

const server = createServer(async (request, response) => {
  const asset = assets.get(new URL(request.url ?? "/", "http://127.0.0.1").pathname);
  if (asset === undefined) {
    response.writeHead(404).end("Not found\n");
    return;
  }
  try {
    const body = await readFile(asset.path);
    response.writeHead(200, {
      "Cache-Control": "no-store",
      "Content-Length": body.byteLength,
      "Content-Type": asset.type,
    });
    response.end(body);
  } catch {
    response.writeHead(500).end("Example asset unavailable\n");
  }
});

server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  if (address === null || typeof address === "string")
    throw new Error("browser server did not bind");
  console.log(`Teslatlas Hub browser consumer: http://127.0.0.1:${address.port}`);
});
