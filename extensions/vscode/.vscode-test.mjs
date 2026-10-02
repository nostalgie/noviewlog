import { defineConfig } from "@vscode/test-cli";

export default defineConfig({
  files: "test/vscode-smoke.cjs",
  mochaOpts: { ui: "bdd" },
  // Disposable throwaway profile; other extensions are excluded.
  launchArgs: ["--disable-extensions", "--profile-temp"],
});
