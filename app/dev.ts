#!/usr/bin/env -S deno run --allow-read --allow-write --allow-net --allow-env --allow-run

import * as esbuild from "https://deno.land/x/esbuild@v0.20.1/mod.js";
import { denoPlugins } from "jsr:@luca/esbuild-deno-loader@^0.10.3";

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
      jsx: "automatic",
      jsxImportSource: "react",
      external: ["*.css"],
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

// Start the file server
const process = new Deno.Command("deno", {
  args: ["serve", "--port", "21173", distDir],
  stdout: "inherit",
  stderr: "inherit",
});

const child = process.spawn();

// Watch for file changes
const watcher = Deno.watchFs(["./src", "./deps.ts"]);

console.log("Watching for file changes...\n");

for await (const event of watcher) {
  if (event.kind === "modify" || event.kind === "create") {
    console.log(`\nFile changed: ${event.paths.join(", ")}`);
    await build();
  }
}

// Cleanup
child.kill();
esbuild.stop();
