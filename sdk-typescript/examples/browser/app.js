import { asEntityTag, createBrowserTransport, readEntityTag } from "/dist/browser.js";

const transport = createBrowserTransport({
  baseUrl: "https://fixture.invalid",
  fetch: async (_input, init) => {
    const headers = new Headers(init?.headers);
    if (headers.get("If-None-Match") !== '"fixture-1"') {
      throw new Error("Example expected its conditional request tag");
    }
    return new Response(null, {
      status: 304,
      headers: { ETag: '"fixture-2"' },
    });
  },
});

const response = await transport.request("/transport-example", {
  ifNoneMatch: asEntityTag('"fixture-1"'),
});
const output = document.querySelector("#output");

if (output === null) {
  throw new Error("Example output element is missing");
}

output.textContent = `Teslatlas SDK browser transport: ${response.status} ${readEntityTag(response.headers)}`;
