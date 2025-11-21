#!/usr/bin/env -S deno run --allow-read --allow-write --allow-net --allow-env --allow-run

import * as esbuild from "https://deno.land/x/esbuild@v0.20.1/mod.js";
import { denoPlugins } from "jsr:@luca/esbuild-deno-loader@^0.10.3";
import { NodeGlobalsPolyfillPlugin } from "npm:@esbuild-plugins/node-globals-polyfill";
import { NodeModulesPolyfillPlugin } from "npm:@esbuild-plugins/node-modules-polyfill";

const distDir = "./dist";
const assetsDir = `${distDir}/assets`;

// Clean dist directory
try {
  await Deno.remove(distDir, { recursive: true });
} catch {
  // Directory might not exist
}

// Create dist and assets directories
await Deno.mkdir(assetsDir, { recursive: true });

console.log("Building React application with esbuild...");

// Bundle the React application
const result = await esbuild.build({
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
  minify: true,
  sourcemap: true,
  target: ["es2020"],
  platform: "browser",
  jsx: "automatic",
  jsxImportSource: "react",
  external: ["*.css"],
  define: {
    "process.env.NODE_ENV": '"production"',
    "global": "window",
  },
  logLevel: "info",
});

console.log("Build completed successfully!");

// Copy CSS file to dist/assets/
console.log("Copying CSS file...");
const cssContent = await Deno.readTextFile("./src/style.css");
await Deno.writeTextFile(`${assetsDir}/style.css`, cssContent);

// Generate index.html
console.log("Generating index.html...");
const htmlContent = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <link rel="icon" type="image/svg+xml" href="/vite.svg" />
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

console.log("Build artifacts created in dist/");
console.log("- dist/index.html");
console.log("- dist/assets/main.js");
console.log("- dist/assets/main.js.map");
console.log("- dist/assets/style.css");

esbuild.stop();
