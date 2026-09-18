# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities privately to the project maintainers through the channel
where you received the project. Do **not** open a public issue for security-sensitive reports.

We aim to acknowledge a report within a few days and will coordinate a fix and disclosure
timeline with you. There is no bug-bounty program.

## Scope

Tare is a **local-first, single-user desktop app + CLI**. It runs on your own machine, stores
data in a local SQLite database, and does not have accounts, multi-tenancy, or a hosted backend.
The most relevant surfaces are:

- The local **read API** and **OTLP receiver**, which bind to loopback (`127.0.0.1`) only.
- The optional **loopback proxy** (`tare serve`), which forwards model traffic to a fixed,
  allowlisted set of provider base URLs.
- Ingestion of **untrusted transcript / OTLP JSON**, which is size-capped and parsed without
  panicking on malformed input.

## Redaction is best-effort (opt-in `max_inspect`)

By default Tare stores only token **counts** and attribution — never your prompts or responses.

The opt-in **`max_inspect`** privacy profile is the exception: it stores request/response
**bodies** locally, in a store separate from the counts database (with `PRAGMA secure_delete`
so a purge overwrites freed pages). Before any body is written, it passes through a redaction
layer (`tare-core/src/redact.rs`) that masks known secret shapes — API keys, Bearer/Basic auth,
`sk-`/`sk_`/`pk_` keys, GitHub/GitLab/Slack/AWS/Google keys, JWTs, PEM private-key blocks, and
emails.

**Honest ceiling:** this is a best-effort scrubber over well-known patterns, **not** a guarantee
that no secret survives. A novel key format or a secret embedded in prose can slip through. Treat
the stored text as "scrubbed of the obvious," not "safe to publish." Redaction deliberately
prefers over-masking (false positives) to leaks (false negatives). The feature is opt-in and the
data never leaves your machine, but you should enable `max_inspect` only when you understand this.

You can inspect the redaction patterns in `tare-core/src/redact.rs` and purge the transcript
store at any time.

## Supported versions

Tare is pre-1.0 and under active development. Security fixes are applied to the latest `main`;
there are no long-term support branches yet.
