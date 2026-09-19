export async function collectDrivePages(client, vehicleId, { limit = 25, maxPages = 128 } = {}) {
  const pages = [];
  const seenCursors = new Set();
  let cursor;

  while (true) {
    const options = { limit };
    if (cursor !== undefined) options.cursor = cursor;
    const page = await client.drives(vehicleId, options);
    if (page.kind !== "page") throw new Error("drive pagination expected a page");
    pages.push(page);

    if (page.value.nextCursor === null) return pages;
    if (seenCursors.has(page.value.nextCursor)) {
      throw new Error("drive pagination repeated a cursor");
    }
    if (pages.length >= maxPages) {
      throw new Error("drive pagination exceeded maximum page count");
    }
    seenCursors.add(page.value.nextCursor);
    cursor = page.value.nextCursor;
  }
}
