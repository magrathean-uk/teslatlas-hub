import { asEntityTag, createNodeTransport, readEntityTag } from "@teslatlas/sdk/node";

const transport = createNodeTransport({
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
const entityTag = readEntityTag(response.headers);

console.log(`Teslatlas SDK transport example: ${response.status} ${entityTag}`);
