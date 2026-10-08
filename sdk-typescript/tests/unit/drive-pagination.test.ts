import { describe, expect, it } from "vitest";
import { collectDrivePages } from "../../scripts/drive-pagination.mjs";

describe("Hub drive pagination helper", () => {
  it("walks a 51-drive history until the terminal cursor", async () => {
    const pages = [
      {
        kind: "page",
        value: { items: Array.from({ length: 25 }, (_, id) => ({ id })), nextCursor: "cursor-25" },
      },
      {
        kind: "page",
        value: {
          items: Array.from({ length: 25 }, (_, id) => ({ id: id + 25 })),
          nextCursor: "cursor-50",
        },
      },
      { kind: "page", value: { items: [{ id: 50 }], nextCursor: null } },
    ] as const;
    const calls: Array<{ cursor?: string; limit?: number }> = [];
    const client = {
      drives: async (_vehicleId: string, options: { cursor?: string; limit?: number }) => {
        calls.push(options);
        const page = pages[calls.length - 1];
        if (page === undefined) throw new Error("unexpected extra page request");
        return page;
      },
    };

    const result = await collectDrivePages(client, "vehicle-1", { limit: 25 });

    expect(result).toHaveLength(3);
    expect(result.flatMap((page) => page.value.items)).toHaveLength(51);
    expect(calls).toEqual([
      { limit: 25 },
      { limit: 25, cursor: "cursor-25" },
      { limit: 25, cursor: "cursor-50" },
    ]);
  });
});
