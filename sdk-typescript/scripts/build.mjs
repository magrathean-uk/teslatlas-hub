import { spawnSync } from "node:child_process";
import {
  copyFile,
  lstat,
  mkdir,
  readFile,
  readdir,
  realpath,
  rm,
  unlink,
  writeFile,
} from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build as bundleBrowser } from "vite";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "..");
const outputArgument = process.argv.indexOf("--out-dir");
const distributionDirectory = resolve(
  outputArgument < 0 ? resolve(repositoryRoot, "dist") : (process.argv[outputArgument + 1] ?? ""),
);
const compiler = resolve(repositoryRoot, "node_modules", "typescript", "bin", "tsc");

if (
  outputArgument >= 0 &&
  (process.argv[outputArgument + 1] === undefined ||
    distributionDirectory === repositoryRoot ||
    relative(repositoryRoot, distributionDirectory).split(/[\\/]/u)[0] !== ".." ||
    relative(distributionDirectory, repositoryRoot).split(/[\\/]/u)[0] !== "..")
) {
  throw new Error("Explicit build output must be outside the repository");
}

if (outputArgument >= 0) {
  const buildRoot = process.env.CLEAN_DEVELOPMENT_BUILD_ROOT;
  if (
    buildRoot === undefined ||
    relative(resolve(buildRoot), distributionDirectory).startsWith("..") ||
    distributionDirectory === resolve(buildRoot)
  ) {
    throw new Error("Explicit output must be below the managed build root");
  }
  const ownerPath = `${distributionDirectory}.teslatlas-sdk-owner.json`;
  const owner = JSON.stringify({
    product: "@teslatlas/sdk",
    sourceRoot: repositoryRoot,
    outputRoot: distributionDirectory,
  });
  const physicalBuildRoot = await realpath(buildRoot);
  let parent = dirname(distributionDirectory);
  let physicalParent;
  while (physicalParent === undefined) {
    try {
      physicalParent = await realpath(parent);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      parent = dirname(parent);
    }
  }
  if (relative(physicalBuildRoot, physicalParent).startsWith("..")) {
    throw new Error("Explicit output parent escapes the managed build root");
  }
  await mkdir(dirname(distributionDirectory), { recursive: true });
  let exists = false;
  try {
    const stat = await lstat(distributionDirectory);
    if (!stat.isDirectory() || stat.isSymbolicLink())
      throw new Error("Build output must be an owned directory");
    exists = true;
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  let marker;
  try {
    const stat = await lstat(ownerPath);
    if (!stat.isFile() || stat.isSymbolicLink())
      throw new Error("Invalid SDK output ownership marker");
    marker = await readFile(ownerPath, "utf8");
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  if (exists || marker !== undefined) {
    if (marker !== owner) {
      throw new Error("Refusing to replace external output without SDK ownership marker");
    }
  } else {
    await writeFile(ownerPath, owner, { flag: "wx" });
  }
}

// Every package must be derived only from current source, including declarations.
await rm(distributionDirectory, { recursive: true, force: true });
const result = spawnSync(
  process.execPath,
  [compiler, "-p", "tsconfig.build.json", "--outDir", distributionDirectory],
  {
    cwd: repositoryRoot,
    stdio: "inherit",
  },
);
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
  // Root is a facade over the standalone browser implementation. Public errors and
  // factories therefore share one constructor even across mixed package imports.
  const rootModule = await readFile(resolve(distributionDirectory, "index.js"), "utf8");
  const rootExports = [...rootModule.matchAll(/export\s*\{([^}]+)\}\s*from\s*[^;]+;/gu)].flatMap(
    (match) =>
      match[1]
        .split(",")
        .map((name) => name.trim())
        .filter(Boolean),
  );
  if (rootExports.length === 0) throw new Error("No compiled public root exports found");
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
  await writeFile(
    resolve(distributionDirectory, "index.js"),
    `export { ${rootExports.join(", ")} } from "./browser.js";\n`,
  );
  await bundleBrowser({
    configFile: false,
    logLevel: "error",
    publicDir: false,
    plugins: [
      {
        name: "shared-public-browser-runtime",
        enforce: "pre",
        resolveId(source, importer) {
          if (source.startsWith("node:")) return { id: source, external: true };
          if (importer === undefined) return;
          const target = resolve(dirname(importer), source).replace(/\.ts$/u, ".js");
          if (
            ["src/browser.js", "src/index.js", "src/core/errors.js", "src/hub/client.js"].some(
              (path) => target === resolve(repositoryRoot, path),
            )
          ) {
            return { id: "./browser.js", external: true };
          }
        },
      },
    ],
    build: {
      emptyOutDir: false,
      lib: {
        entry: resolve(repositoryRoot, "src/node.ts"),
        fileName: () => "node.js",
        formats: ["es"],
      },
      outDir: distributionDirectory,
      rollupOptions: { output: { codeSplitting: false } },
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
