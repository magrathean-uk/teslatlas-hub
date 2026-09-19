import { describe, expect, it, vi } from "vitest";
import { browserAndPageCdp } from "../../scripts/browser-hub-runner.mjs";

describe("browser runner CDP target ownership", () => {
  it("uses separate browser and page sessions for command-line and Network domains", async () => {
    const browserCdp = { send: vi.fn() };
    const networkCdp = { send: vi.fn() };
    const page = {};
    const context = {
      newPage: vi.fn().mockResolvedValue(page),
      newCDPSession: vi.fn().mockResolvedValue(networkCdp),
    };
    const browser = {
      newBrowserCDPSession: vi.fn().mockResolvedValue(browserCdp),
      contexts: vi.fn().mockReturnValue([context]),
      newContext: vi.fn(),
    };

    await expect(browserAndPageCdp(browser)).resolves.toEqual({ browserCdp, page, networkCdp });
    expect(browser.newBrowserCDPSession).toHaveBeenCalledOnce();
    expect(context.newCDPSession).toHaveBeenCalledWith(page);
    expect(browserCdp).not.toBe(networkCdp);
  });
});
