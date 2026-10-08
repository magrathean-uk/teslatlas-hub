import { playwright } from "@vitest/browser-playwright";
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: "unit",
          include: ["tests/unit/**/*.test.ts"],
          environment: "node",
        },
      },
      {
        test: {
          name: "node-conformance",
          include: ["tests/conformance/node.test.ts"],
          environment: "node",
        },
      },
      {
        test: {
          name: "browser-conformance",
          include: ["tests/conformance/browser.test.ts"],
          browser: {
            enabled: true,
            headless: true,
            provider: playwright(),
            instances: [{ browser: "chromium" }],
          },
        },
      },
      {
        test: {
          name: "protocol-cases",
          include: ["tests/protocol/**/*.test.ts"],
          environment: "node",
        },
      },
    ],
  },
});
