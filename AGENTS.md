# AGENTS.md — llm-gateway

A small Rust service between self-hosted services and LLM providers (Mistral first). It holds the provider keys and enforces the budget, rate limits, retries, usage accounting per service and sphere, and the spheres' rules about providers.

## Status

Batch embeddings from Mistral (through the `genai` crate) with caller keys, usage accounting and a monthly budget (#1), retries, a rate limit per caller and sphere rules (#2), and the owner's usage view (#3) are implemented; the API, the view and the configuration are in `README.md`. Further work is in the GitHub issues. The surrounding architecture (one service per kind of thing, spheres, a central configuration) is documented privately with the server's infrastructure in `../server-infra/docs/assistant/` (ADR 0003 there is this gateway's reason to exist).

## Architecture

One crate, `crates/llm-gateway` (library plus the `llm-gateway` binary):

- `api`: the axum HTTP API and its error answers (`{"error": {"code", ...}}`).
- `Gateway` (`lib.rs`): validates a request, checks the sphere's rules, the budget and the caller's rate limit, calls the provider with retries, records usage and refusals. The clock is injected so tests can move between months.
- `spheres`: reads the spheres file, the part of the central configuration that server-infra's `config-apply` renders (format: `../server-infra/docs/assistant/config-format.md`, version 1). It ignores keys it does not need, as that format allows, and rejects versions it does not know.
- `limits`: the rate limit, a token bucket of texts per caller. `retry`: the retry policy (exponential backoff or the provider's `Retry-After`, within a time limit); both use tokio's clock, so tests run them on paused time.
- `provider`: the `Provider` trait and `Mistral`, which calls `genai` with an explicit `ServiceTarget` (endpoint, key and adapter), so genai never guesses from model names or the environment. Provider errors are transient (retried) or permanent.
- `usage`: the SQLite usage database (`usage`, `refusal` and `session` tables, migrations in `crates/llm-gateway/migrations/`). `report`: the usage view's numbers for a month, as plain data with tests. `sessions`: the owner's browser sessions (only token hashes are stored).
- `web`: the usage view on its own listener, so only it is exposed to browsers. askama templates in `crates/llm-gateway/templates/`, one stylesheet in `assets/` and SVG charts drawn on the server (`web::chart`); no JavaScript. `web::login` and `web::oidc` are the owner's OIDC login, adapted from mnemograph's web login; both are meant to move into the assistant's shared crates.
- `money`: exact amounts in nano-euros and prices per model; `callers`: caller keys, kept as SHA-256 digests.

Tests use a fake `Provider` (`tests/embeddings.rs`), a fake Mistral HTTP API (`tests/mistral.rs`) and a fake OIDC provider for the login (`tests/web.rs`); one ignored test calls the real API when `LLM_GATEWAY_MISTRAL_API_KEY` is set.

## Design constraints

- Only the gateway holds provider keys; callers authenticate to it with their own key.
- Callers decide what to process and when; the gateway never queues work for them, it answers or refuses (budget exhausted, rule violated, rate limited).
- Lean like the other services: Rust, SQLite, a small container.
- This repo is public: no hostnames, keys or personal configuration; those come from the central configuration at runtime.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for `michael-fuchs/llm-gateway` (public), managed via `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` + `docs/adr/` at the repo root, created when the first term or decision is settled. See `docs/agents/domain.md`.
