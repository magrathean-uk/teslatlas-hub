import { spawnSync } from "node:child_process";
import { copyFile, readFile, readdir, rm, unlink, writeFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build as bundleBrowser } from "vite";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "..");
const distributionDirectory = resolve(repositoryRoot, "dist");
const compiler = resolve(repositoryRoot, "node_modules", "typescript", "bin", "tsc");

if (relative(repositoryRoot, distributionDirectory) !== "dist") {
  throw new Error("Build output directory is not the repository dist directory");
}

// Every package must be derived only from current source, including declarations.
await rm(distributionDirectory, { recursive: true, force: true });
const result = spawnSync(process.execPath, [compiler, "-p", "tsconfig.build.json"], {
  cwd: repositoryRoot,
  stdio: "inherit",
});
if (result.error !== undefined) throw result.error;
if (result.status !== 0) {
  process.exitCode = result.status ?? 1;
} else {
  await copyFile(
    resolve(repositoryRoot, "src/generated/hub-validators.js"),
    resolve(distributionDirectory, "generated/hub-validators.js"),
  );
  // tsc reads .d.ts inputs but does not emit them into outDir.
  await copyFile(
    resolve(repositoryRoot, "src/generated/hub-validators.d.ts"),
    resolve(distributionDirectory, "generated/hub-validators.d.ts"),
  );
  await preserveGeneratedNoCheckDirectives();
  await bundleBrowser({
    configFile: false,
    logLevel: "error",
    publicDir: false,
    build: {
      emptyOutDir: false,
      lib: {
        entry: resolve(repositoryRoot, "src/browser.ts"),
        fileName: () => "browser.js",
        formats: ["es"],
      },
      outDir: distributionDirectory,
      rollupOptions: {
        output: { codeSplitting: false },
      },
      sourcemap: false,
      target: "es2022",
    },
  });
  await removeSourceMaps(distributionDirectory);
}

async function preserveGeneratedNoCheckDirectives() {
  const sourceDirectory = resolve(repositoryRoot, "src/generated");
  const declarationDirectory = resolve(distributionDirectory, "generated");
  const sourceEntries = await readdir(sourceDirectory, { withFileTypes: true });
  await Promise.all(
    sourceEntries
      .filter((entry) => entry.isFile() && entry.name.endsWith(".ts"))
      .map(async (entry) => {
        const source = await readFile(resolve(sourceDirectory, entry.name), "utf8");
        if (!source.startsWith("// @ts-nocheck\n")) return;
        const declarationPath = resolve(
          declarationDirectory,
          `${entry.name.slice(0, -".ts".length)}.d.ts`,
        );
        const declaration = await readFile(declarationPath, "utf8");
        if (!declaration.startsWith("// @ts-nocheck\n")) {
          await writeFile(declarationPath, `// @ts-nocheck\n${declaration}`);
        }
      }),
  );
}

async function removeSourceMaps(directory) {
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") return;
    throw error;
  }
  await Promise.all(
    entries.map(async (entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) {
        await removeSourceMaps(path);
      } else if (entry.isFile() && entry.name.endsWith(".map")) {
        await unlink(path);
      }
    }),
  );
}
