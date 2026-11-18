// Starts the page. A file of its own, not an inline script: the page's policy allows only its
// own files to run (docs/specs/automations.md §B3).
import init from "./irori-tesla-app.js";
init({ module_or_path: new URL("./irori-tesla-app_bg.wasm", import.meta.url) });
