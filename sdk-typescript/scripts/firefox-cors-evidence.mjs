const preflightVisibility = "not-exposed-by-playwright-firefox-request-events";

export async function observeFirefoxCorsReads(context, appOrigin, hubEndpoint) {
  const page = await context.newPage();
  let hubRequestsObserved = 0;
  const observeRequest = (request) => {
    if (request.url().startsWith(hubEndpoint) && request.method() !== "OPTIONS") {
      hubRequestsObserved += 1;
    }
  };
  context.on("request", observeRequest);
  try {
    await page.goto(appOrigin.href, { waitUntil: "load" });
    await page.waitForFunction(() => window.__teslatlasResult !== undefined, null, {
      timeout: 30_000,
    });
    const result = await page.evaluate(() => window.__teslatlasResult);
    if (!result?.ok) throw new Error(result?.error ?? "browser route failed");
    if (result.pageOrigin !== appOrigin.origin) throw new Error("browser page origin differs");
    if (result.defaultFetch !== true) throw new Error("browser reads did not use default Fetch");
    if (hubRequestsObserved === 0) throw new Error("no cross-origin Hub requests were observed");
    return {
      ...result,
      cors: {
        browserEnforced: true,
        hubRequestsObserved,
        preflightVisibility,
      },
      runtime: context.browser().version(),
    };
  } finally {
    context.off("request", observeRequest);
    await page.close();
  }
}
