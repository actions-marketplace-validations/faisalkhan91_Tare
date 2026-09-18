// Node 24 exposes an incomplete global `localStorage` unless it receives a
// --localstorage-file flag. Vitest 2 preserves that host accessor instead of replacing it with
// jsdom's per-test storage object, so explicitly bind the browser globals to the active jsdom
// window. This keeps tests isolated and matches the runtime API.
const memoryStorage = (): Storage => {
  const values = new Map<string, string>();
  return {
    get length() { return values.size; },
    clear: () => values.clear(),
    getItem: (key) => values.get(String(key)) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => { values.delete(String(key)); },
    setItem: (key, value) => { values.set(String(key), String(value)); },
  };
};
const jsdomLocalStorage = memoryStorage();
const jsdomSessionStorage = memoryStorage();

Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: jsdomLocalStorage,
});
Object.defineProperty(globalThis, "sessionStorage", {
  configurable: true,
  value: jsdomSessionStorage,
});

// Browsers treat an anchor with `download` as a file download instead of a page
// navigation. jsdom does not implement downloads, so its default click action
// incorrectly falls through to its unsupported navigation path. Preserve the
// browser contract while still letting the production export handler create,
// click, and remove the real anchor during tests.
document.addEventListener(
  "click",
  (event) => {
    const target = event.target;
    if (target instanceof Element && target.closest("a[download]")) {
      event.preventDefault();
    }
  },
  { capture: true }
);
