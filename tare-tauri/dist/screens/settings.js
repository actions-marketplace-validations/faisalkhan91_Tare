// Settings utility content: seven stable, keyboard-native sections spanning device-local
// preferences, tare.toml capture configuration, and durable local saved investigations. The modal
// sheet shell is owned by ui/sheet + shell/workbench so this renderer stays directly testable.
import { el } from "../ui/el.js";
import { icon } from "../ui/icon.js";
import { currentOs } from "../ui/os.js";
import { humanizeKey } from "../ui/format.js";
import { privacyProfileDescription } from "../ui/privacyProfiles.js";
import { errorNode } from "../ui/errorNode.js";
import { refreshMs, setRefreshMs, density, setDensity, applyDensity, setOnboarded, showBaselineDelta, setShowBaselineDelta, defaultScreen, setDefaultScreen, } from "../ui/prefs.js";
import { formatShortcut } from "../ui/keymap.js";
import { themePreference, setThemePreference, applyTheme, } from "../ui/theme.js";
// Mirrors the Rust `Profile` enum. `max_inspect` is the only profile that stores any
// body text — redacted + isolated in a separate DB + purgeable — so it's offered last, after the
// no-text profiles, and paired with the purge control below.
const PROFILES = ["strict_counts", "fingerprint", "max_private", "max_inspect"];
function num(v) {
    const n = Number(v);
    return v.trim() === "" || !Number.isFinite(n) ? undefined : n;
}
function str(v) {
    return v.trim() === "" ? undefined : v.trim();
}
/// Recursively drop unset leaves (undefined/null/"") and now-empty objects, so the "non-defaults"
/// raw-config view shows only fields the user actually set. Pure; returns a new object.
function pruneEmpty(v) {
    if (Array.isArray(v))
        return v;
    if (v && typeof v === "object") {
        const out = {};
        for (const [k, val] of Object.entries(v)) {
            const pruned = pruneEmpty(val);
            const empty = pruned === undefined ||
                pruned === null ||
                pruned === "" ||
                (typeof pruned === "object" && !Array.isArray(pruned) && Object.keys(pruned).length === 0);
            if (!empty)
                out[k] = pruned;
        }
        return out;
    }
    return v;
}
export async function renderSettings(root, client, initialSection) {
    root.replaceChildren(el("p", { class: "skeleton", text: "Loading settings…" }));
    let cfg;
    try {
        cfg = await client.config();
    }
    catch (e) {
        root.replaceChildren(errorNode("Couldn't load settings. Check that tare.toml is readable.", e, {
            actions: [{
                    label: "Retry",
                    primary: true,
                    run: () => renderSettings(root, client, initialSection),
                }],
        }));
        return;
    }
    const canSave = client.canSaveConfig?.() ?? false;
    let investigations = null;
    try {
        investigations = await client.listInvestigations();
    }
    catch {
        // Settings remains usable when the local database is temporarily unavailable; that section
        // states the failure without pretending the authoritative list is empty.
    }
    // Per-field provenance: default | tare.toml | env. Empty on transports that can't
    // resolve it (the browser can't see server env).
    const origins = await client.configOrigins().catch(() => ({}));
    // A capture-config field that shows where its effective value comes from, and disables the input
    // when an env var overrides it (so the form can't lie: editing it wouldn't take).
    const capField = (label, control, path) => {
        const origin = origins[path];
        if (origin === "env") {
            control.disabled = true;
        }
        const labelText = origin === "env"
            ? "From env (read-only here)"
            : origin === "tare.toml"
                ? "From tare.toml"
                : origin === "default"
                    ? "App default"
                    : "Effective config · tare.toml or app default";
        return field(label, control, labelText, origin ? `origin-${origin === "tare.toml" ? "toml" : origin}` : "origin-effective");
    };
    const frag = document.createDocumentFragment();
    // Declarative alert rules: a mutable list the user edits; saved into config.alert.
    const alertRules = (cfg.alert ?? []).map((r) => ({ ...r }));
    const alertList = el("div", { class: "alert-rules" });
    const renderAlertRules = () => {
        if (alertRules.length === 0) {
            alertList.replaceChildren(el("span", { class: "sub", text: "No custom rules. The built-in budget (75%/100%) + any-anomaly alerts are always on." }));
            return;
        }
        alertList.replaceChildren(...alertRules.map((r, i) => el("div", { class: "alert-rule-row" }, [
            el("span", {
                class: "sub",
                text: r.metric === "anomaly_kind"
                    ? `${humanizeKey(r.metric)} = ${r.kind ? humanizeKey(r.kind) : "any"}`
                    : `${humanizeKey(r.metric)} ≥ ${r.threshold ?? "?"}${r.metric === "period_pct" ? "%" : ""}${r.min_events ? ` (min ${r.min_events} events)` : ""}`,
            }),
            el("button", {
                class: "nav-view-del",
                "aria-label": `Remove alert rule ${i + 1}`,
                title: "Remove rule",
                onClick: () => {
                    alertRules.splice(i, 1);
                    renderAlertRules();
                },
            }, [icon("close", { size: 12 })]),
        ])));
    };
    renderAlertRules();
    const ruleMetric = el("select", { class: "input", "aria-label": "Alert metric" }, ["today_spend", "period_pct", "run_rate", "anomaly_kind"].map((m) => el("option", { value: m, text: humanizeKey(m) })));
    const ruleValue = el("input", { class: "input", type: "text", placeholder: "Threshold or kind", "aria-label": "Alert threshold or anomaly kind" });
    const addRule = el("button", {
        class: "btn ghost",
        text: "Add rule",
        onClick: () => {
            const m = ruleMetric.value;
            const raw = ruleValue.value.trim();
            if (m === "anomaly_kind") {
                alertRules.push({ metric: m, kind: raw || "any" });
            }
            else {
                const t = Number(raw);
                if (!Number.isFinite(t) || t <= 0)
                    return;
                alertRules.push({ metric: m, threshold: t });
            }
            ruleValue.value = "";
            renderAlertRules();
        },
    });
    const alertSection = el("section", { class: "section settings-section", "data-settings-section": "alerts" }, [
        el("h2", {}, ["Alerts"]),
        sectionOrigin("Saved in tare.toml · evaluated locally"),
        el("h3", { class: "subhead" }, ["Alert rules"]),
        el("p", { class: "caption", text: "Local-first alerts (toast + OS notification, never webhooks). Rules augment the always-on budget + anomaly defaults. Use dollars for today_spend/run_rate thresholds and percentages for period_pct." }),
        alertList,
        canSave ? el("div", { class: "diff-controls" }, [ruleMetric, ruleValue, addRule]) : el("span", { class: "sub", text: "Editing requires the desktop app; edit [[alert]] in tare.toml on the browser." }),
    ]);
    // Local-model cost overlay: a user-supplied per-backend $/1M tokens so self-hosted runs
    // read as a real (estimated, user-owned) figure instead of $0. Saved into providers.local_overlay.
    const overlays = (cfg.providers.local_overlay ?? []).map((o) => ({ ...o }));
    const overlayList = el("div", { class: "local-overlays" });
    const renderOverlays = () => {
        if (overlays.length === 0) {
            overlayList.replaceChildren(el("span", { class: "sub", text: "No overlay. Self-hosted runs are shown as usage-only until you add a backend rate." }));
            return;
        }
        overlayList.replaceChildren(...overlays.map((o, i) => {
            const rate = o.usd_per_mtok != null
                ? `$${o.usd_per_mtok}/1M tokens`
                : o.kwh_per_mtok != null && o.usd_per_kwh != null
                    ? `${o.kwh_per_mtok} kWh/1M tokens at $${o.usd_per_kwh}/kWh`
                    : "no rate";
            return el("div", { class: "alert-rule-row" }, [
                el("span", { class: "sub", text: `${o.backend}: ${rate} (your estimate)` }),
                el("button", {
                    class: "nav-view-del",
                    "aria-label": `Remove overlay ${i + 1}`,
                    title: "Remove overlay",
                    onClick: () => {
                        overlays.splice(i, 1);
                        renderOverlays();
                    },
                }, [icon("close", { size: 12 })]),
            ]);
        }));
    };
    renderOverlays();
    const ovBackend = el("select", { class: "input", "aria-label": "Local backend" }, ["ollama", "vllm", "llama.cpp", "tgi", "lmstudio", "localai"].map((b) => el("option", { value: b, text: b })));
    const ovRate = el("input", { class: "input", type: "text", placeholder: "$ / 1M tokens", "aria-label": "USD per 1M tokens" });
    const addOverlay = el("button", {
        class: "btn ghost",
        text: "Add overlay",
        onClick: () => {
            const usd = Number(ovRate.value.trim());
            if (!Number.isFinite(usd) || usd < 0)
                return;
            overlays.push({ backend: ovBackend.value, usd_per_mtok: usd });
            ovRate.value = "";
            renderOverlays();
        },
    });
    const overlayPanel = el("div", { class: "settings-subsection" }, [
        el("h3", { class: "subhead" }, ["Local model cost overlay"]),
        el("p", { class: "caption", text: "Self-hosted backends (Ollama, vLLM, llama.cpp, …) start as usage-only. Attach your own $/1M tokens estimate so their cost is comparable. This is clearly your figure, not a provider invoice. Per-backend identity is preserved automatically." }),
        overlayList,
        canSave ? el("div", { class: "diff-controls" }, [ovBackend, ovRate, addOverlay]) : el("span", { class: "sub", text: "Editing requires the desktop app; edit [[providers.local_overlay]] in tare.toml on the browser." }),
    ]);
    // ---- App preferences (always editable; localStorage) ----
    // Appearance is the ONLY place theme is chosen: a System/Light/Dark tristate
    // over the ThemePreference. "System" tracks the OS live (via watchOsTheme); Light/Dark are explicit
    // overrides. No toggle command / topbar control / native menu item exists anymore.
    const themeSel = selectEl(["system", "light", "dark"], themePreference(), (o) => o.charAt(0).toUpperCase() + o.slice(1));
    themeSel.addEventListener("change", () => {
        applyTheme(setThemePreference(themeSel.value));
    });
    const refreshInput = inputEl("number", String(Math.round(refreshMs() / 1000)));
    refreshInput.min = "1";
    refreshInput.max = "3600";
    refreshInput.step = "1";
    refreshInput.addEventListener("change", () => {
        const secs = Math.min(3600, Math.max(1, Math.round(num(refreshInput.value) ?? 3)));
        setRefreshMs(secs * 1000);
        refreshInput.value = String(secs); // reflect the clamp so the field matches refreshMs()
    });
    // Replay onboarding: clears the first-run flag so the guided setup shows again next launch.
    const replayBtn = el("button", {
        class: "btn ghost",
        text: "Replay onboarding",
        onClick: () => {
            setOnboarded(false);
            replayBtn.textContent = "Will show on next launch ✓";
        },
    });
    // Tare has one visual identity; this setting only opts shell affordances into the OS accent.
    const densitySel = selectEl(["comfortable", "compact"], density());
    densitySel.addEventListener("change", () => {
        const d = densitySel.value;
        applyDensity(d);
        setDensity(d);
    });
    // Runs workbench: show/hide the "Change vs baseline" column.
    const baselineDeltaToggle = el("input", { type: "checkbox" });
    baselineDeltaToggle.checked = showBaselineDelta();
    baselineDeltaToggle.addEventListener("change", () => setShowBaselineDelta(baselineDeltaToggle.checked));
    // Default landing screen: which screen opens on launch when there's no deep link.
    const SCREENS = [
        ["pulse", "Pulse"],
        ["investigate", "Investigate"],
        ["optimize", "Optimize"],
    ];
    const screenSel = el("select", { class: "input" }, SCREENS.map(([v, l]) => el("option", { value: v, text: l })));
    screenSel.value = defaultScreen();
    screenSel.addEventListener("change", () => setDefaultScreen(screenSel.value));
    // (No separate "default time range" control: the topbar range picker already persists rangePref, so
    // it IS the remembered default — a second control here would silently desync from the topbar.)
    // Background-on-close: surfaced ONLY on Windows/Linux desktop — macOS always keeps
    // running and the browser has no window-close-to-tray. Persisted via the desktop close-policy
    // file (background.json), which the Rust close policy reads on the next window close.
    let bgCloseField = false;
    if (client.canBackgroundOnClose() && currentOs() !== "macos") {
        const bgClose = el("input", { type: "checkbox" });
        void client.getBackgroundOnClose().then((on) => {
            bgClose.checked = on;
        });
        bgClose.addEventListener("change", () => void client.setBackgroundOnClose(bgClose.checked));
        bgCloseField = field("Close keeps Tare running in the background", bgClose, "Desktop close policy · stored on this device");
    }
    const appearanceSection = el("section", { class: "section settings-section", "data-settings-section": "appearance" }, [
        el("h2", {}, ["Appearance"]),
        sectionOrigin("Stored on this device"),
        field("Theme", themeSel, "Stored on this device"),
        field("Density", densitySel, "Stored on this device"),
    ]);
    const behaviorSection = el("section", { class: "section settings-section", "data-settings-section": "behavior" }, [
        el("h2", {}, ["Behavior"]),
        sectionOrigin("Stored on this device unless noted"),
        field("Default screen", screenSel, "Stored on this device"),
        field("Live refresh (seconds)", refreshInput, "Stored on this device"),
        field("Show change vs baseline in Runs", baselineDeltaToggle, "Stored on this device"),
        bgCloseField,
        field("Onboarding", replayBtn, "Stored on this device"),
    ]);
    // ---- Capture config (tare.toml) ----
    const maxSpend = inputEl("number", cfg.budget.max_spend_usd?.toString() ?? "");
    const maxSteps = inputEl("number", cfg.budget.max_steps?.toString() ?? "");
    const maxRepeats = inputEl("number", cfg.budget.max_repeats?.toString() ?? "");
    const period = selectEl(["(none)", "week", "month"], cfg.budget.period ?? "(none)");
    const periodMax = inputEl("number", cfg.budget.period_max_spend_usd?.toString() ?? "");
    const warnPct = inputEl("number", cfg.budget.warn_pct?.toString() ?? "");
    // Guard against drift from the Rust `Profile` enum: if the stored profile isn't one we know,
    // keep it as an extra option rather than silently snapping the select to the first entry.
    const knownProfiles = cfg.privacy.profile && !PROFILES.includes(cfg.privacy.profile)
        ? [...PROFILES, cfg.privacy.profile]
        : PROFILES;
    // Humanize the option labels like the sibling selects, keeping the raw enum values.
    const profile = selectEl(["(default)", ...knownProfiles], cfg.privacy.profile ?? "(default)", (o) => o === "(default)" ? "(default)" : humanizeKey(o));
    // One-line, point-of-use description of the selected profile's tradeoff (updates live on change).
    const profileDesc = el("p", { class: "caption sub privacy-profile-desc" });
    const syncProfileDesc = () => {
        const v = profile.value === "(default)" ? "strict_counts" : profile.value;
        profileDesc.textContent = privacyProfileDescription(v);
    };
    syncProfileDesc();
    profile.addEventListener("change", syncProfileDesc);
    const salt = inputEl("text", cfg.privacy.salt ?? "");
    const anthropicUp = inputEl("text", cfg.providers.anthropic_upstream ?? "");
    const openaiUp = inputEl("text", cfg.providers.openai_upstream ?? "");
    const geminiUp = inputEl("text", cfg.providers.gemini_upstream ?? "");
    const azureUp = inputEl("text", cfg.providers.azure_openai_upstream ?? "");
    const bedrockUp = inputEl("text", cfg.providers.bedrock_upstream ?? "");
    const port = inputEl("number", cfg.proxy.port?.toString() ?? "");
    const otlpPort = inputEl("number", cfg.proxy.otlp_port?.toString() ?? "");
    const pricing = inputEl("text", cfg.proxy.pricing ?? "");
    const db = inputEl("text", cfg.proxy.db ?? "");
    // When capture runs: the "run all the time vs only when the app is open" choice.
    const CAPTURE_MODES = ["app_only", "always_on", "off"];
    const storedMode = cfg.capture?.mode && CAPTURE_MODES.includes(cfg.capture.mode) ? cfg.capture.mode : "app_only";
    const captureMode = selectEl(CAPTURE_MODES, storedMode, humanizeKey);
    const CAPTURE_MODE_DESC = {
        app_only: "Captures only while Tare is open, and catches up everything since you last opened it so nothing is lost.",
        always_on: "Installs a background login item so capture keeps running across restarts, even with the window closed.",
        off: "Capture is disabled.",
    };
    const captureModeDesc = el("p", { class: "caption sub" });
    const syncCaptureModeDesc = () => {
        captureModeDesc.textContent = CAPTURE_MODE_DESC[captureMode.value] ?? "";
    };
    syncCaptureModeDesc();
    captureMode.addEventListener("change", syncCaptureModeDesc);
    // Independent per-intake JSONL toggle: default on; lets a user keep OTel capture up
    // while switching the ~/.claude JSONL lane off, without disabling capture entirely.
    const jsonlEnabled = el("input", { type: "checkbox" });
    jsonlEnabled.checked = cfg.capture?.jsonl ?? true;
    const anomalyWindow = inputEl("number", cfg.anomaly?.window?.toString() ?? "");
    const anomalyThreshold = inputEl("number", cfg.anomaly?.threshold?.toString() ?? "");
    const tzOffset = inputEl("number", cfg.ui?.tz_offset_minutes?.toString() ?? "");
    // Reprice mode + preserved overrides: the UI edits `reprice`; `overrides` round-trip
    // untouched so a Settings save never wipes `[pricing]` (overrides stay editable in tare.toml).
    const heldOverrides = cfg.pricing?.overrides;
    const repriceSel = el("select", { class: "input", "aria-label": "Reprice mode" }, [
        el("option", { value: "as-of", text: "As-of (each edition at its date)", ...(cfg.pricing?.reprice !== "latest" ? { selected: "" } : {}) }),
        el("option", { value: "latest", text: "Latest (reprice all at newest edition)", ...(cfg.pricing?.reprice === "latest" ? { selected: "" } : {}) }),
    ]);
    // role=status (aria-live polite) so "saved ✓" / errors are announced to assistive tech.
    const status = el("span", { class: "save-status", role: "status" });
    const saveBtn = el("button", {
        class: "btn",
        text: "Save tare.toml settings",
        onClick: () => void save(),
    });
    if (!canSave)
        saveBtn.disabled = true;
    // opt-out: one-click purge of every captured body (the max_inspect escape hatch).
    // Two-step inline confirm (arm → confirm) so a stray click can't silently wipe capture history.
    const purgeStatus = el("span", { class: "save-status", role: "status" });
    let purgeArmed = false;
    const purgeBtn = el("button", {
        class: "btn",
        text: "Purge captured bodies",
    });
    purgeBtn.addEventListener("click", () => {
        if (!purgeArmed) {
            purgeArmed = true;
            purgeBtn.textContent = "Click again to confirm purge";
            purgeStatus.textContent = "This permanently deletes all captured request/response bodies.";
            return;
        }
        purgeArmed = false;
        purgeBtn.textContent = "Purge captured bodies";
        purgeStatus.textContent = "Purging…";
        client
            .purgeTranscripts()
            .then(() => {
            purgeStatus.textContent = "All captured bodies purged.";
        })
            .catch(() => {
            purgeStatus.textContent = "Couldn't purge. Try again.";
        });
    });
    const purgeField = el("div", { class: "purge-field" }, [
        el("p", {
            class: "caption",
            text: "The Max inspect profile stores redacted request/response bodies in a separate, purgeable store; every other profile is counts-only.",
        }),
        el("div", { class: "diff-controls" }, [purgeBtn, purgeStatus]),
    ]);
    // Raw config view: the config maps 1:1 to tare.toml; a read-only pretty dump + a
    // "non-defaults only" filter makes the form inspectable and copy-pasteable for backup. The DTO
    // already reflects only set fields (serde skip-if-none), so non-defaults ≈ defined values.
    const rawPre = el("pre", { class: "explain raw-config" });
    let nonDefaultsOnly = false;
    const renderRaw = () => {
        rawPre.textContent = JSON.stringify(nonDefaultsOnly ? pruneEmpty(cfg) : cfg, null, 2);
    };
    const ndToggle = el("input", { type: "checkbox", "aria-label": "Show only non-default config fields" });
    ndToggle.addEventListener("change", () => {
        nonDefaultsOnly = ndToggle.checked;
        renderRaw();
    });
    const copyRaw = el("button", {
        class: "btn ghost",
        text: "Copy",
        onClick: () => void navigator.clipboard?.writeText(rawPre.textContent ?? ""),
    });
    renderRaw();
    const rawConfig = el("details", { class: "raw-config-wrap" }, [
        el("summary", {}, ["Raw config"]),
        el("div", { class: "diff-controls" }, [
            el("label", { class: "sub" }, [ndToggle, " Show only non-defaults"]),
            copyRaw,
        ]),
        rawPre,
    ]);
    const capture = el("section", { class: "section settings-section", "data-settings-section": "privacy-capture" }, [
        el("h2", {}, ["Privacy & Capture"]),
        sectionOrigin(canSave ? "Saved in tare.toml · capture stays local" : "Read from tare.toml · browser viewer is read-only"),
        el("p", {
            class: "caption",
            text: canSave
                ? "The desktop app and CLI honor the same file. Prompt and response text is never sent anywhere by Settings."
                : "Read-only in the browser; edit capture settings in the desktop app or tare.toml. Device-local Appearance and Behavior settings remain editable.",
        }),
        el("h3", { class: "subhead" }, ["When to capture"]),
        field("Capture mode", captureMode, "Stored in tare.toml"),
        captureModeDesc,
        // Privacy transparency: the JSONL lane is the zero-setup default, so state plainly
        // what it reads. It is structurally counts-only under every privacy profile — no prompt/response
        // text is ever read or stored — so there's nothing to leak; say so where capture is configured.
        el("p", { class: "caption sub jsonl-privacy-note" }, [
            el("strong", { text: "What capture reads: " }),
            "Claude Code JSONL session files under ~/.claude: token counts and model/timing metadata only, never prompt or response text, read-only, and nothing leaves this machine.",
        ]),
        field("Read Claude Code JSONL session files", jsonlEnabled, "Stored in tare.toml"),
        el("p", { class: "caption sub" }, [
            "On by default. This is the zero-setup lane. Turn it off to stop reading those JSONL session files while keeping other capture, such as the OTel channel, running.",
        ]),
        capField("Privacy profile", profile, "privacy.profile"),
        profileDesc,
        purgeField,
        el("details", { class: "advanced" }, [
            el("summary", {}, ["Advanced"]),
            capField("Privacy salt", salt, "privacy.salt"),
            capField("Anthropic upstream URL", anthropicUp, "providers.anthropic_upstream"),
            capField("OpenAI upstream URL", openaiUp, "providers.openai_upstream"),
            capField("Gemini upstream URL", geminiUp, "providers.gemini_upstream"),
            capField("Azure OpenAI upstream URL", azureUp, "providers.azure_openai_upstream"),
            capField("Bedrock upstream URL", bedrockUp, "providers.bedrock_upstream"),
            field("Proxy port", port, "Stored in tare.toml"),
            field("OTel channel port", otlpPort, "Stored in tare.toml"),
        ]),
    ]);
    alertSection.append(el("h3", { class: "subhead" }, ["Anomaly detection"]), el("p", { class: "caption", text: "Defaults for the local spend-anomaly alarm (window 7 days, threshold 50%)." }), field("Window (days)", anomalyWindow, "Stored in tare.toml"), field("Threshold (%)", anomalyThreshold, "Stored in tare.toml"));
    const dataSection = el("section", { class: "section settings-section", "data-settings-section": "data" }, [
        el("h2", {}, ["Data"]),
        sectionOrigin("Configuration stays on this device"),
        el("h3", { class: "subhead" }, ["Budget & kill-switch"]),
        capField("Max spend (USD)", maxSpend, "budget.max_spend_usd"),
        capField("Max steps", maxSteps, "budget.max_steps"),
        capField("Max identical repeats", maxRepeats, "budget.max_repeats"),
        field("Budget period", period, "Stored in tare.toml"),
        field("Period spend cap (USD)", periodMax, "Stored in tare.toml"),
        field("Warn at (% of period cap)", warnPct, "Stored in tare.toml"),
        overlayPanel,
        el("h3", { class: "subhead" }, ["Storage & pricing"]),
        field("Pricing table path", pricing, "Stored in tare.toml"),
        field("Reprice mode", repriceSel, "Stored in tare.toml"),
        capField("Database path", db, "proxy.db"),
        el("p", { class: "caption", text: "Local-day offset in minutes for 'today' (e.g. -480 = UTC−8). Blank = UTC." }),
        capField("Timezone offset (min)", tzOffset, "ui.tz_offset_minutes"),
        rawConfig,
        el("div", { class: "diff-controls settings-save" }, [saveBtn, status]),
    ]);
    const shortcutRows = [
        ["Open Settings", formatShortcut("Mod+,")],
        ["Open Commands", formatShortcut("Mod+K")],
        ["Find in page", formatShortcut("Mod+F")],
        ["Focus search", "/"],
        ["Open application menu", "F10 · Linux"],
    ];
    const shortcutsSection = el("section", { class: "section settings-section", "data-settings-section": "shortcuts" }, [
        el("h2", {}, ["Shortcuts"]),
        sectionOrigin("Built in · follows native OS modifier conventions"),
        el("dl", { class: "settings-shortcuts" }, shortcutRows.flatMap(([action, shortcut]) => [
            el("dt", { text: action }),
            el("dd", {}, [el("kbd", { text: shortcut })]),
        ])),
    ]);
    const investigationList = el("div", { class: "saved-investigation-list" });
    const investigationStatus = el("p", { class: "save-status", role: "status" });
    const renderInvestigations = () => {
        if (investigations === null) {
            investigationList.replaceChildren(el("p", { class: "sub", text: "Couldn't read the local investigation database. Retry by reopening Settings." }));
            return;
        }
        if (investigations.length === 0) {
            investigationList.replaceChildren(el("p", { class: "sub", text: "No saved investigations yet. Save one from Commands while investigating." }));
            return;
        }
        investigationList.replaceChildren(...investigations.map((investigation) => {
            const remove = el("button", {
                class: "btn ghost saved-investigation-delete",
                "aria-label": `Delete saved investigation ${investigation.label}`,
                text: "Delete",
                onClick: () => {
                    remove.disabled = true;
                    investigationStatus.textContent = `Deleting ${investigation.label}…`;
                    void client
                        .deleteInvestigation(investigation.id)
                        .then(() => {
                        investigations = investigations?.filter((row) => row.id !== investigation.id) ?? [];
                        investigationStatus.textContent = "Saved investigation deleted.";
                        globalThis.dispatchEvent?.(new CustomEvent("tare:investigations-changed"));
                        renderInvestigations();
                    })
                        .catch(() => {
                        remove.disabled = false;
                        investigationStatus.textContent = `Couldn't delete ${investigation.label}.`;
                    });
                },
            });
            return el("div", { class: "saved-investigation-row" }, [
                el("span", { class: "saved-investigation-summary" }, [
                    el("strong", { text: investigation.label }),
                    el("span", {
                        class: "field-origin",
                        text: "Local SQLite",
                    }),
                ]),
                remove,
            ]);
        }));
    };
    renderInvestigations();
    const savedInvestigationsSection = el("section", { class: "section settings-section", "data-settings-section": "saved-investigations" }, [
        el("h2", {}, ["Saved investigations"]),
        sectionOrigin("Tare's local SQLite database · never synced by Tare"),
        investigationList,
        investigationStatus,
    ]);
    if (!canSave) {
        for (const inp of [
            maxSpend,
            maxSteps,
            maxRepeats,
            period,
            periodMax,
            warnPct,
            captureMode,
            jsonlEnabled,
            profile,
            salt,
            anthropicUp,
            openaiUp,
            geminiUp,
            azureUp,
            bedrockUp,
            port,
            otlpPort,
            pricing,
            db,
            anomalyWindow,
            anomalyThreshold,
            tzOffset,
            repriceSel,
            ruleMetric,
            ruleValue,
            addRule,
            ovBackend,
            ovRate,
            addOverlay,
        ]) {
            inp.disabled = true;
        }
    }
    const settingsSections = [
        { key: "appearance", label: "Appearance", section: appearanceSection },
        { key: "behavior", label: "Behavior", section: behaviorSection },
        { key: "privacy-capture", label: "Privacy & Capture", section: capture },
        { key: "alerts", label: "Alerts", section: alertSection },
        { key: "data", label: "Data", section: dataSection },
        { key: "shortcuts", label: "Shortcuts", section: shortcutsSection },
        { key: "saved-investigations", label: "Investigations", section: savedInvestigationsSection },
    ];
    const requestedKey = settingsSections.some(({ key }) => key === initialSection)
        ? initialSection
        : "appearance";
    const navButtons = [];
    const settingsNav = el("nav", {
        class: "settings-nav",
        id: "settings-section-nav",
        "aria-label": "Settings sections",
    }, settingsSections.map(({ key, label, section }) => {
        section.id = `settings-section-${key}`;
        const heading = section.querySelector("h2");
        if (heading)
            heading.tabIndex = -1;
        const button = el("button", {
            class: "settings-nav-button",
            type: "button",
            "aria-controls": section.id,
            ...(key === requestedKey ? { "aria-current": "location" } : {}),
            text: label,
            onClick: () => {
                for (const item of navButtons)
                    item.removeAttribute("aria-current");
                button.setAttribute("aria-current", "location");
                section.scrollIntoView?.({ block: "start" });
                heading?.focus({ preventScroll: true });
            },
        });
        navButtons.push(button);
        return button;
    }));
    const earlierSections = el("button", {
        class: "settings-nav-scroll settings-nav-scroll-back",
        type: "button",
        "aria-label": "Show earlier settings sections",
        "aria-controls": "settings-section-nav",
        title: "Earlier settings sections",
        hidden: "",
    }, [icon("chevron", { size: 14 })]);
    const laterSections = el("button", {
        class: "settings-nav-scroll settings-nav-scroll-forward",
        type: "button",
        "aria-label": "Show later settings sections",
        "aria-controls": "settings-section-nav",
        title: "More settings sections",
        hidden: "",
    }, [icon("chevron", { size: 14 })]);
    const settingsNavShell = el("div", {
        class: "settings-nav-shell",
        "data-overflow": "false",
    }, [earlierSections, settingsNav, laterSections]);
    const updateNavOverflow = () => {
        const maxScroll = Math.max(0, settingsNav.scrollWidth - settingsNav.clientWidth);
        const overflows = maxScroll > 1;
        settingsNavShell.setAttribute("data-overflow", String(overflows));
        earlierSections.hidden = !overflows || settingsNav.scrollLeft <= 1;
        laterSections.hidden = !overflows || settingsNav.scrollLeft >= maxScroll - 1;
    };
    const scrollSections = (direction) => {
        const distance = Math.max(160, settingsNav.clientWidth * 0.72);
        const reduceMotion = globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
        settingsNav.scrollBy?.({ left: direction * distance, behavior: reduceMotion ? "auto" : "smooth" });
    };
    earlierSections.addEventListener("click", () => scrollSections(-1));
    laterSections.addEventListener("click", () => scrollSections(1));
    settingsNav.addEventListener("scroll", updateNavOverflow, { passive: true });
    frag.append(settingsNavShell, ...settingsSections.map(({ section }) => section));
    root.replaceChildren(frag);
    // The renderer builds off-DOM, so overflow can only be measured after insertion. ResizeObserver
    // keeps the affordance honest across sheet/viewport resizing; the frame covers initial layout.
    if (typeof ResizeObserver !== "undefined") {
        const observer = new ResizeObserver(() => {
            if (!settingsNav.isConnected) {
                observer.disconnect();
                return;
            }
            updateNavOverflow();
        });
        observer.observe(settingsNav);
    }
    if (typeof requestAnimationFrame === "function")
        requestAnimationFrame(updateNavOverflow);
    else
        queueMicrotask(updateNavOverflow);
    const requestedSection = settingsSections.find(({ key }) => key === requestedKey)?.section;
    if (initialSection && requestedSection) {
        queueMicrotask(() => requestedSection.scrollIntoView?.({ block: "start" }));
    }
    async function save() {
        const next = {
            // Spread the LOADED config first so every section this sheet does not edit survives the save
            // Saving is a whole-file rewrite, so building `next` from scratch deleted
            // `[[unit]]` and `[[lineage]]` outright — and the same trap would catch the next section
            // anyone adds to TareConfig. The per-section spreads below (anomaly, pricing) predate this
            // and are now redundant, but harmless and left explicit for readability.
            ...cfg,
            budget: {
                // Preserve advanced config-only fields such as `soft_spend_usd`; this form does not edit
                // them, so a routine Settings save must not erase them.
                ...cfg.budget,
                max_spend_usd: num(maxSpend.value),
                max_steps: num(maxSteps.value),
                max_repeats: num(maxRepeats.value),
                period: period.value === "(none)" ? undefined : period.value,
                period_max_spend_usd: num(periodMax.value),
                warn_pct: num(warnPct.value),
            },
            privacy: {
                // `suppress_latency` and `git_attribution` are valid privacy settings even though this
                // compact form does not expose them. Keep both across a whole-file rewrite.
                ...cfg.privacy,
                profile: profile.value === "(default)" ? undefined : profile.value,
                salt: str(salt.value),
            },
            providers: {
                anthropic_upstream: str(anthropicUp.value),
                openai_upstream: str(openaiUp.value),
                gemini_upstream: str(geminiUp.value),
                azure_openai_upstream: str(azureUp.value),
                bedrock_upstream: str(bedrockUp.value),
                local_overlay: overlays.length > 0 ? overlays : undefined,
            },
            proxy: {
                port: num(port.value),
                db: str(db.value),
                otlp_port: num(otlpPort.value),
                pricing: str(pricing.value),
            },
            // Spread the loaded anomaly config so a Settings save (which edits only window/threshold)
            // never wipes the noise floors or the acknowledged-anomaly list.
            anomaly: { ...cfg.anomaly, window: num(anomalyWindow.value), threshold: num(anomalyThreshold.value) },
            ui: { tz_offset_minutes: num(tzOffset.value) },
            capture: { mode: captureMode.value, jsonl: jsonlEnabled.checked },
            alert: alertRules.length > 0 ? alertRules : undefined,
            // Round-trip pricing: set the chosen reprice mode + preserve overrides verbatim so
            // saving Settings never wipes `[pricing]`.
            pricing: {
                reprice: repriceSel.value === "latest" ? "latest" : "as-of",
                overrides: heldOverrides,
            },
        };
        // Disable while the write is in flight to prevent double-submit.
        saveBtn.disabled = true;
        status.textContent = "Saving…";
        // Single error sink: clear any prior save error so repeated failures don't stack up.
        dataSection.querySelectorAll(".save-error").forEach((n) => n.remove());
        try {
            await client.saveConfig(next);
            status.textContent = "Saved ✓";
        }
        catch (e) {
            status.textContent = "";
            const err = errorNode("Couldn't save settings. Check that the configured port is free.", e, {
                actions: [{ label: "Retry save", primary: true, run: () => saveBtn.click() }],
            });
            err.classList.add("save-error");
            dataSection.appendChild(err);
        }
        finally {
            saveBtn.disabled = false;
        }
    }
}
function sectionOrigin(text) {
    return el("p", { class: "settings-origin", text });
}
function field(label, control, origin = "Stored in tare.toml", originClass = "origin-toml") {
    return el("label", { class: "field" }, [
        el("span", { class: "field-label", text: label }),
        control,
        el("span", { class: `field-origin ${originClass}`, text: origin }),
    ]);
}
function inputEl(type, value) {
    const i = el("input", { type });
    i.value = value;
    return i;
}
function selectEl(options, value, labelOf = (o) => o) {
    const s = el("select", {}, options.map((o) => el("option", { value: o, text: labelOf(o) })));
    s.value = value;
    return s;
}
