#!/usr/bin/env -S deno run --allow-read --allow-write --allow-net --allow-env --allow-run

import * as esbuild from "https://deno.land/x/esbuild@v0.20.1/mod.js";
import { denoPlugins } from "jsr:@luca/esbuild-deno-loader@^0.10.3";
import { serveDir } from "jsr:@std/http@1.0.10/file-server";
import { NodeGlobalsPolyfillPlugin } from "npm:@esbuild-plugins/node-globals-polyfill";
import { NodeModulesPolyfillPlugin } from "npm:@esbuild-plugins/node-modules-polyfill";

const distDir = "./dist";
const assetsDir = `${distDir}/assets`;

// Create dist and assets directories if they don't exist
try {
  await Deno.mkdir(assetsDir, { recursive: true });
} catch {
  // Directory already exists
}

console.log("Starting development build...");

// Initial build
async function build() {
  console.log("Building...");

  try {
    await esbuild.build({
      plugins: [
        NodeGlobalsPolyfillPlugin({
          process: true,
          buffer: true,
        }),
        NodeModulesPolyfillPlugin(),
        ...denoPlugins({
          configPath: Deno.cwd() + "/deno.json",
        }),
      ],
      entryPoints: ["./src/main.tsx"],
      outfile: `${assetsDir}/main.js`,
      bundle: true,
      format: "esm",
      minify: false,
      sourcemap: true,
      target: ["es2020"],
      platform: "browser",
      jsx: "automatic",
      jsxImportSource: "react",
      external: ["*.css"],
      define: {
        "process.env.NODE_ENV": '"development"',
        "global": "window",
      },
      logLevel: "info",
    });

    // Copy CSS file
    const cssContent = await Deno.readTextFile("./src/style.css");
    await Deno.writeTextFile(`${assetsDir}/style.css`, cssContent);

    // Generate index.html
    const htmlContent = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Audio Stream Server</title>
    <link rel="stylesheet" href="/assets/style.css" />
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/assets/main.js"></script>
  </body>
</html>
`;

    await Deno.writeTextFile(`${distDir}/index.html`, htmlContent);
    console.log("Build completed!");
  } catch (error) {
    console.error("Build failed:", error);
  }
}

// Run initial build
await build();

console.log("\nStarting file server on http://localhost:21173");
console.log("Serving from ./dist directory\n");

// Start the file server in a separate async context
const serverPromise = Deno.serve({
  port: 21173,
  onListen: () => {
    console.log("Server ready at http://localhost:21173");
    console.log("Watching for file changes...\n");
  },
}, (req) => {
  return serveDir(req, {
    fsRoot: distDir,
    showDirListing: false,
    enableCors: true,
  });
});

// Watch for file changes
const watcher = Deno.watchFs(["./src", "./deps.ts"]);

try {
  for await (const event of watcher) {
    if (event.kind === "modify" || event.kind === "create") {
      console.log(`\nFile changed: ${event.paths.join(", ")}`);
      await build();
    }
  }
} finally {
  // Cleanup
  await serverPromise;
  esbuild.stop();
}
