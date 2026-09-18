// Browser entry point. Loaded as an external module (`script-src 'self'`-safe) rather than an
// inline script, so it works under a strict CSP. Wires the shell to the loopback HTTP read API.
import { mountApp } from "./main.js";
import { createHttpClient } from "./httpClient.js";
import { cachingClient } from "./ui/cachingClient.js";
const root = document.getElementById("app");
if (root) {
    mountApp(root, cachingClient(createHttpClient())).catch((e) => {
        root.textContent = "Failed to start: " + String(e);
    });
}
