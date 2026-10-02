import * as esbuild from "esbuild";

/** @type {import('esbuild').BuildOptions} */
const extension = {
  entryPoints: ["src/extension.ts"],
  bundle: true,
  outfile: "dist/extension.js",
  platform: "node",
  format: "cjs",
  target: "node20",
  sourcemap: true,
  external: [
    "vscode",
    "@lydell/node-pty",
    "./wasm/noviewlog_wasm.js",
  ],
};

/** @type {import('esbuild').BuildOptions} */
const webview = {
  entryPoints: ["src/webview/main.ts"],
  bundle: true,
  outfile: "dist/webview.js",
  platform: "browser",
  format: "iife",
  target: "es2022",
  sourcemap: true,
};

const watch = process.argv.includes("--watch");

if (watch) {
  const ctx = await esbuild.context(extension);
  const webCtx = await esbuild.context(webview);
  await Promise.all([ctx.watch(), webCtx.watch()]);
  console.log("watching...");
} else {
  await esbuild.build(extension);
  await esbuild.build(webview);
  console.log("built dist/extension.js + dist/webview.js");
}
