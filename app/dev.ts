#!/usr/bin/env -S deno run --allow-read --allow-write --allow-net --allow-env --allow-run

import * as esbuild from "https://deno.land/x/esbuild@v0.20.1/mod.js";
import { denoPlugins } from "jsr:@luca/esbuild-deno-loader@^0.10.3";
import { NodeGlobalsPolyfillPlugin } from "npm:@esbuild-plugins/node-globals-polyfill";
import { NodeModulesPolyfillPlugin } from "npm:@esbuild-plugins/node-modules-polyfill";
import { extname } from "https://deno.land/std@0.221.0/path/mod.ts";

const PORT = 21173;
const distDir = "./dist-dev";
const assetsDir = `${distDir}/assets`;
const entryPoint = "./src/main.tsx";
const cssSource = "./src/style.css";

let cleaned = false;
let building = false;
let pendingRebuild = false;

const htmlContent = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <link rel="icon" type="image/svg+xml" href="/vite.svg" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Audio Stream Server (dev)</title>
    <link rel="stylesheet" href="/assets/style.css" />
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/assets/main.js"></script>
  </body>
</html>
`;

function contentType(path: string): string {
  switch (extname(path)) {
    case ".html":
      return "text/html; charset=utf-8";
    case ".js":
      return "application/javascript";
    case ".css":
      return "text/css";
    case ".map":
      return "application/json";
    default:
      return "application/octet-stream";
  }
}

async function ensureDirs() {
  if (!cleaned) {
    try {
      await Deno.remove(distDir, { recursive: true });
    } catch {
      // directory might not exist yet
    }
    cleaned = true;
  }

  await Deno.mkdir(assetsDir, { recursive: true });
}

async function writeHtml() {
  await Deno.writeTextFile(`${distDir}/index.html`, htmlContent);
}

async function copyCss() {
  const css = await Deno.readTextFile(cssSource);
  await Deno.writeTextFile(`${assetsDir}/style.css`, css);
}

async function buildBundle() {
  await ensureDirs();

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
    entryPoints: [entryPoint],
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

  await copyCss();
  await writeHtml();

  console.log(`[dev] build completed at ${new Date().toLocaleTimeString()}`);
}

async function rebuild() {
  if (building) {
    pendingRebuild = true;
    return;
  }

  building = true;
  try {
    await buildBundle();
  } catch (error) {
    console.error("[dev] build failed:", error);
  } finally {
    building = false;
    if (pendingRebuild) {
      pendingRebuild = false;
      await rebuild();
    }
  }
}

function startServer() {
  console.log(`[dev] serving ${distDir} on http://localhost:${PORT}`);

  Deno.serve({ hostname: "0.0.0.0", port: PORT }, async (req) => {
    const url = new URL(req.url);
    let filePath: string | null = null;

    if (url.pathname === "/") {
      filePath = `${distDir}/index.html`;
    } else if (url.pathname.startsWith("/assets/")) {
      filePath = `${distDir}${url.pathname}`;
    }

    if (!filePath) {
      return new Response("Not Found", { status: 404 });
    }

    try {
      const data = await Deno.readFile(filePath);
      return new Response(data, {
        status: 200,
        headers: {
          "content-type": contentType(filePath),
        },
      });
    } catch {
      return new Response("Not Found", { status: 404 });
    }
  });
}

async function watchSource() {
  console.log("[dev] watching ./src for changes...");
  const watcher = Deno.watchFs("./src");
  let debounceTimer: number | null = null;

  for await (const event of watcher) {
    if (event.kind === "access") continue;

    const relativePaths = event.paths.map((p) => p.replace(`${Deno.cwd()}/`, ""));
    console.log(`[dev] change detected (${event.kind}): ${relativePaths.join(", ")}`);

    if (debounceTimer !== null) {
      clearTimeout(debounceTimer);
    }
    debounceTimer = setTimeout(async () => {
      debounceTimer = null;
      await rebuild();
    }, 100) as unknown as number;
  }
}

addEventListener("unload", () => {
  esbuild.stop();
});

await rebuild();
startServer();
await watchSource();
