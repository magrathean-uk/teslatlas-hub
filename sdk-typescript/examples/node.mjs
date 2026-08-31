import { createClient } from "@teslatlas/sdk/node";
import discovery from "../protocol/source/examples/discovery.json" with { type: "json" };
import vehicles from "../protocol/source/examples/vehicles-page.json" with { type: "json" };

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

if (result.kind !== "modified") {
  throw new Error("Fixture vehicle response was unexpectedly not modified");
}

console.log(
  `Teslatlas SDK Node client: ${result.value.items.length} vehicle, protocol ${client.protocolVersion}`,
);
