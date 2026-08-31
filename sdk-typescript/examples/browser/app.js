import { createClient } from "/dist/browser.js";

const [discovery, vehicles] = await Promise.all([
  fetch("/fixtures/discovery.json").then((response) => response.json()),
  fetch("/fixtures/vehicles-page.json").then((response) => response.json()),
]);
const client = await createClient({
  baseUrl: "https://fixture.invalid",
  authorization: () => undefined,
  fetch: async (input) => {
    const path = new URL(String(input)).pathname;
    if (path === "/.well-known/teslatlas-hub") {
      return Response.json(discovery, { headers: { ETag: 'W/"fixture-discovery-1"' } });
    }
    if (path === "/v1/vehicles") {
      return Response.json(vehicles, { headers: { ETag: 'W/"fixture-vehicles-1"' } });
    }
    throw new Error(`Unexpected fixture path ${path}`);
  },
});
const result = await client.listVehicles();
const output = document.querySelector("#output");

if (output === null) {
  throw new Error("Example output element is missing");
}
if (result.kind !== "modified") {
  throw new Error("Fixture vehicle response was unexpectedly not modified");
}

output.textContent = `Teslatlas SDK browser client: ${result.value.items.length} vehicle, protocol ${client.protocolVersion}`;
