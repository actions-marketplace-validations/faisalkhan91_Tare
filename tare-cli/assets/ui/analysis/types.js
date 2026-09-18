// Canonical CohortSpec DTOs — a byte-exact mirror of the serde wire form
// emitted by `tare-core/src/cohort.rs`. snake_case field names; internally-tagged unions match the
// Rust `#[serde(tag = ...)]` tags (`op` / `mode` / `kind`). Canonicalization + hashing live in
// `serialize.ts`; these are the shapes that cross the wire and get hashed.
export {};
