import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, extname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const exampleDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(exampleDirectory, "../..");
const distDirectory = resolve(repositoryRoot, "dist");
const port = Number.parseInt(process.env.TESLATLAS_EXAMPLE_PORT ?? "4173", 10);

if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) {
  throw new Error("TESLATLAS_EXAMPLE_PORT must be an integer from 0 to 65535");
}

const server = createServer(async (request, response) => {
  try {
    const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
    const file = resolveAsset(requestUrl.pathname);
    if (file === undefined) {
      response.writeHead(404).end("Not found\n");
      return;
    }

    const body = await readFile(file);
    response.writeHead(200, {
      "Content-Type": contentType(file),
      "Content-Length": body.byteLength,
      "Cache-Control": "no-store",
    });
    response.end(body);
  } catch {
    response.writeHead(500).end("Example server error\n");
  }
});

server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  if (address === null || typeof address === "string") {
    throw new Error("Browser example server has no TCP address");
  }
  console.log(`Teslatlas SDK browser example: http://127.0.0.1:${address.port}`);
});

function resolveAsset(pathname) {
  if (pathname === "/") {
    return resolve(exampleDirectory, "index.html");
  }
  if (pathname === "/app.js") {
    return resolve(exampleDirectory, "app.js");
  }
  if (!pathname.startsWith("/dist/")) {
    return undefined;
  }

  const candidate = resolve(repositoryRoot, `.${decodeURIComponent(pathname)}`);
  const pathFromDist = relative(distDirectory, candidate);
  if (pathFromDist.startsWith(`..${sep}`) || pathFromDist === ".." || isAbsolute(pathFromDist)) {
    return undefined;
  }
  return candidate;
}

function contentType(file) {
  switch (extname(file)) {
    case ".html":
      return "text/html; charset=utf-8";
    case ".js":
      return "text/javascript; charset=utf-8";
    case ".map":
      return "application/json; charset=utf-8";
    default:
      return "application/octet-stream";
  }
}
