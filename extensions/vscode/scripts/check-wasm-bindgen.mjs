// Fails with a clear message when the installed wasm-bindgen-cli does not
// match the version locked in the workspace Cargo.lock (wasm-bindgen glue
// is version-sensitive and any mismatch breaks build:wasm).
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";

const lock = readFileSync("../../Cargo.lock", "utf8");
const match = /name = "wasm-bindgen"\r?\nversion = "([^"]+)"/.exec(lock);
if (!match) {
  console.error("wasm-bindgen not found in Cargo.lock");
  process.exit(1);
}
const expected = match[1];
let installed;
try {
  installed = execSync("wasm-bindgen --version", { encoding: "utf8" }).trim();
} catch {
  console.error(
    `wasm-bindgen-cli is not installed. Install the locked version:\n  cargo install wasm-bindgen-cli --version ${expected} --locked`,
  );
  process.exit(1);
}
if (!installed.includes(expected)) {
  console.error(
    `wasm-bindgen-cli ${installed} does not match the Cargo.lock version ${expected}.\nReinstall:\n  cargo install wasm-bindgen-cli --version ${expected} --locked`,
  );
  process.exit(1);
}
console.log(`wasm-bindgen-cli ${expected} OK`);
