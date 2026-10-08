import { describe, expect, it } from "vitest";
import { asEntityTag, asOpaqueCursor } from "../../src/core/opaque-values.js";
import { InvalidRequestPathError } from "../../src/http/fetch-transport.js";
import {
  buildReadRequest,
  interpolatePath,
  readOperationDescriptors,
} from "../../src/http/request-builder.js";

describe("closed read request builder", () => {
  it("root-relatively interpolates every and only declared path value", () => {
    expect(interpolatePath("/v1/vehicles/{vehicle_id}", { vehicle_id: "vehicle/demo" })).toBe(
      "/v1/vehicles/vehicle%2Fdemo",
    );
    expect(() => interpolatePath("/v1/vehicles/{vehicle_id}", {})).toThrow(InvalidRequestPathError);
    expect(() =>
      interpolatePath("/v1/vehicles/{vehicle_id}", {
        vehicle_id: "vehicle_demo",
        undeclared: "secret",
      }),
    ).toThrow(InvalidRequestPathError);
  });

  it("preserves opaque query bytes and appends declared arrays repeatedly", () => {
    const request = buildReadRequest(
      {
        pathTemplate: "/v1/events",
        queryNames: ["cursor", "event_type"],
        versioned: true,
      },
      {},
      {
        cursor: asOpaqueCursor("opaque+/="),
        event_type: ["drive.started", "drive.ended"],
      },
      "1.2.0",
    );
    const url = new URL(request.path, "https://hub.example.invalid");

    expect(url.searchParams.get("cursor")).toBe("opaque+/=");
    expect(url.searchParams.getAll("event_type")).toEqual(["drive.started", "drive.ended"]);
  });

  it("drops undeclared query values and emits only protocol-owned headers", () => {
    const request = buildReadRequest(
      readOperationDescriptors.listVehicles,
      {},
      {
        cursor: asOpaqueCursor("opaque_cursor_0001"),
        undeclared: "must-not-send",
      },
      "1.2.0",
      asEntityTag('W/"revision-1"'),
    );

    expect(request.path).toBe("/v1/vehicles?cursor=opaque_cursor_0001");
    expect([...new Headers(request.init.headers).entries()]).toEqual([
      ["teslatlas-protocol-version", "1.2.0"],
    ]);
    expect(request.init.ifNoneMatch).toBe('W/"revision-1"');
  });

  it("keeps discovery unversioned", () => {
    const request = buildReadRequest(
      readOperationDescriptors.discoverHub,
      {},
      {},
      "1.2.0",
      asEntityTag('"discovery-1"'),
    );

    expect(request.path).toBe("/.well-known/teslatlas-hub");
    expect([...new Headers(request.init.headers).entries()]).toEqual([]);
    expect(request.init.ifNoneMatch).toBe('"discovery-1"');
  });
});
