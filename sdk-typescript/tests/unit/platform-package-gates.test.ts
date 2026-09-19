import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import Ajv2020Module from "ajv/dist/2020.js";
import addFormatsModule from "ajv-formats";
import { describe, expect, it } from "vitest";

const dockerfileUrl = new URL("../../Dockerfile", import.meta.url);

describe("F3/F6 platform and package gates", () => {
  it("pins the official Node index and its linux/arm64 child", async () => {
    const dockerfile = await readFile(dockerfileUrl, "utf8");
    const imageLock = JSON.parse(
      await readFile(new URL("../../tools/node-image-lock.json", import.meta.url), "utf8"),
    );

    expect(imageLock).toMatchObject({
      registry: "registry-1.docker.io",
      repository: "library/node",
      official_image: true,
      index_digest: "sha256:4db36457f406501e6f608802e5da617e5fbd0e80b75901b6a09de1ae5a667d32",
      linux_arm64_v8_child_digest:
        "sha256:2b028cd57303b2761d24173789c85a013558d6cf20e78f51723385f368b6e34d",
    });
    expect(dockerfile).toContain(
      `node:26.7.0-bookworm-slim@${imageLock.index_digest} AS package-check`,
    );
    expect(dockerfile).toContain("FROM --platform=linux/arm64");
    expect(dockerfile).toContain('test "$BUILDPLATFORM" = "linux/arm64"');
    expect(dockerfile).toContain('test "$TARGETPLATFORM" = "linux/arm64"');
    expect(dockerfile).toContain('test "$(uname -m)" = "aarch64"');
  });

  it("keeps the Docker build package-only", async () => {
    const dockerfile = await readFile(dockerfileUrl, "utf8");
    const copyLines = dockerfile.split("\n").filter((line) => line.startsWith("COPY "));

    expect(copyLines).toEqual([
      "COPY teslatlas-sdk.tgz /tmp/teslatlas-sdk.tgz",
      "COPY package-smoke.mjs ./package-smoke.mjs",
      "COPY consumer/package.json consumer/node.mjs consumer/index.html consumer/app.js consumer/serve.mjs ./",
    ]);
    expect(dockerfile).not.toMatch(/COPY (src|protocol|scripts|tests|docs|package-lock\.json)/u);
    expect(dockerfile).toContain("SDK_TARBALL_SHA256");
  });

  it("encodes only the runtime evidence that is already accepted", async () => {
    const support = JSON.parse(
      await readFile(new URL("../../tools/platform-support.json", import.meta.url), "utf8"),
    );
    const manifest = JSON.parse(
      await readFile(new URL("../../package.json", import.meta.url), "utf8"),
    );

    expect(support.reproducibility_toolchain).toEqual({ node: "26.7.0", npm: "11.19.0" });
    expect(support.accepted_runtime_evidence).toEqual([
      {
        os: "macOS 27",
        architecture: "arm64",
        node: "26.7.0",
        npm: "11.19.0",
        browser: "Chrome 153.0.8010.52",
      },
    ]);
    expect(support.declared_floors).toEqual({ node: null, npm: null, browser: null });
    expect(manifest.teslatlasSupport).toEqual({
      policy: support.policy,
      reproducibilityToolchain: support.reproducibility_toolchain,
      acceptedRuntimeEvidence: support.accepted_runtime_evidence,
      declaredFloors: support.declared_floors,
    });
    expect(manifest.files).toEqual(
      expect.arrayContaining([
        "docs/package-lifecycle.md",
        "tools/f3-f6-catalog.schema.json",
        "tools/node-image-lock.json",
        "tools/package-admission.schema.json",
        "tools/platform-support.json",
      ]),
    );
  });

  it("keeps provenance files stable JSON inputs", async () => {
    for (const path of ["node-image-lock.json", "platform-support.json"]) {
      const bytes = await readFile(new URL(`../../tools/${path}`, import.meta.url));
      expect(createHash("sha256").update(bytes).digest("hex")).toMatch(/^[0-9a-f]{64}$/u);
      expect(JSON.parse(bytes.toString("utf8"))).toBeTypeOf("object");
    }
  });

  it("compiles both admission schemas in strict Ajv mode without warnings", async () => {
    const warnings: string[] = [];
    const ajv = new Ajv2020Module.default({
      strict: true,
      logger: {
        error: () => undefined,
        log: () => undefined,
        warn: (message: unknown) => warnings.push(String(message)),
      },
    });
    addFormatsModule.default(ajv);

    for (const name of ["package-admission.schema.json", "f3-f6-catalog.schema.json"]) {
      const schema = JSON.parse(
        await readFile(new URL(`../../tools/${name}`, import.meta.url), "utf8"),
      );
      expect(() => ajv.compile(schema)).not.toThrow();
    }
    expect(warnings).toEqual([]);
  });
});
