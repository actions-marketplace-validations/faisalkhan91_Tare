# Route and navigation contract

Tare uses a hash router so the same deep links work in the browser viewer and the desktop WebView.
The current analytical destinations are Pulse, Investigate, and Optimize; utility views open as
sheets without replacing the active workspace.

## Canonical routes

| Hash | Surface |
| --- | --- |
| `#/pulse` | Current spend, budget, forecast, and attention queue |
| `#/pulse?mode=now` | Active sessions and recent expensive steps |
| `#/investigate` | Cohort explorer |
| `#/investigate/run/:id` | Run profile |
| `#/investigate/compare` | Run or cohort comparison |
| `#/investigate?mode=timeline` | Timeline |
| `#/investigate?mode=distinguish` | Configuration correlations |
| `#/investigate?entity=templates&mode=lineage` | Prompt lineage |
| `#/investigate?view=units` | Work units |
| `#/optimize` | Savings opportunities and verification |
| `#/optimize?view=scenarios` | Offline scenarios |
| `#/pulse?sheet=capture` | Capture utility sheet |
| `#/pulse?sheet=trust` | Trust, pricing, and receipts sheet |
| `#/pulse?sheet=settings` | Settings utility sheet |
| `#/onboarding` | First-run setup |

Each path segment is encoded independently. A run ID containing `/`, for example, remains one
segment when encoded as `%2F`. Query keys are sorted during serialization and empty values are
dropped, giving equivalent routes one stable hash.

## Compatibility redirects

Old links remain usable, but they do not load retired standalone screen implementations.
`web/src/ui/routes.ts` maps them directly to the corresponding canonical state:

| Previous hash | Canonical destination |
| --- | --- |
| `#/live` | `#/pulse?mode=now` |
| `#/overview` | `#/pulse` |
| `#/runs` | `#/investigate?entity=runs` |
| `#/runs/:id` | `#/investigate/run/:id` |
| `#/sessions` | `#/investigate?entity=sessions` |
| `#/trends` | `#/investigate?mode=timeline` |
| `#/segments` | `#/investigate?view=facets` |
| `#/correlate` | `#/investigate?mode=distinguish` |
| `#/lineage` | `#/investigate?entity=templates&mode=lineage` |
| `#/units` | `#/investigate?view=units&metric=spend_micros&norm=per_outcome` |
| `#/compare`, `#/diff` | `#/investigate/compare` |
| `#/experiments`, `#/whatif` | `#/optimize?view=scenarios` |
| `#/advise` | `#/optimize?type=cache` |
| `#/receipts` | `#/pulse?sheet=trust` |
| `#/pricing` | `#/pulse?sheet=trust&view=pricing` |
| `#/connect` | `#/pulse?sheet=capture` |
| `#/settings` | `#/pulse?sheet=settings` |

Unrelated query values are preserved through redirects, including selected run IDs in `runs=` and
the timeline dimension in `by=`. Unknown route names are not guessed; the shell shows a not-found
state with a link back to Pulse.

## State and native navigation

Analysis scope, filters, selection, baseline, metric, normalization, and workspace modes are encoded
in the route query by `web/src/analysis/serialize.ts`. Saved investigations persist the durable state
in SQLite; transient focus is reconstructed when a view opens.

The native Go menu and tray emit the same canonical hashes through `tare-tauri/src/gui.rs` and
`web/src/bootTauri.ts`. Recent runs open `#/investigate/run/:id`; Settings opens as a sheet over the
current workspace.

Router unit tests cover segment encoding, query preservation, malformed input, and every redirect.
Browser tests additionally open every canonical surface and compatibility hash and assert that it
renders without a visible error or not-found state.
