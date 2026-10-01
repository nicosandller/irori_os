// Starts the page. A file of its own, not an inline script: the page's policy allows only its
// own files to run (docs/specs/automations.md §B3).
import init from "./irori-automations-app.js";
init({ module_or_path: new URL("./irori-automations-app_bg.wasm", import.meta.url) });
