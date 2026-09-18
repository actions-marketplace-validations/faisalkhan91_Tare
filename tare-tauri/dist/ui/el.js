// Tiny typed DOM builder — framework-free, runs identically in the Tauri WebView and jsdom.
// Untrusted strings go through `textContent`/text nodes by DEFAULT (structural XSS guard);
// the only path to innerHTML is the explicit `rawSvg` helper, reserved for the trusted,
// byte-stable SVG the Rust core also emits.
/// Create an element. `class`/`text` are special-cased; `onClick`/`onChange` attach listeners;
/// everything else becomes an attribute. `text` and string children are inserted as text nodes.
export function el(tag, attrs = {}, children = []) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
        if (v === undefined || v === false)
            continue;
        if (k === "onClick")
            node.addEventListener("click", v);
        else if (k === "onChange")
            node.addEventListener("change", v);
        else if (k === "class")
            node.className = String(v);
        else if (k === "text")
            node.textContent = String(v);
        else
            node.setAttribute(k, String(v));
    }
    for (const c of children) {
        if (c === null || c === undefined || c === false)
            continue;
        node.appendChild(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return node;
}
/// Set a host element's contents to a TRUSTED SVG string (the byte-stable renderer output).
/// This is the single sanctioned innerHTML sink in the app.
export function rawSvg(host, svg) {
    host.innerHTML = svg;
    return host;
}
export function clear(node) {
    node.replaceChildren();
}
