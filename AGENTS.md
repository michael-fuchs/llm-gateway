# AGENTS.md — llm-gateway

A small Rust service between self-hosted services and LLM providers (Mistral first). It holds the provider keys and enforces the budget, rate limits, retries, usage accounting per service and sphere, and the spheres' rules about providers.

## Status

Batch embeddings from Mistral (through the `genai` crate) with caller keys, usage accounting and a monthly budget (#1), and retries, a rate limit per caller and sphere rules (#2) are implemented; the API and configuration are in `README.md`. The owner's usage view (#3) is next; the work is in the GitHub issues. The surrounding architecture (one service per kind of thing, spheres, a central configuration) is documented privately with the server's infrastructure in `../server-infra/docs/assistant/` (ADR 0003 there is this gateway's reason to exist).

## Architecture

One crate, `crates/llm-gateway` (library plus the `llm-gateway` binary):

- `api`: the axum HTTP API and its error answers (`{"error": {"code", ...}}`).
- `Gateway` (`lib.rs`): validates a request, checks the sphere's rules, the budget and the caller's rate limit, calls the provider with retries, records usage and refusals. The clock is injected so tests can move between months.
- `spheres`: reads the spheres file, the part of the central configuration that server-infra's `config-apply` renders (format: `../server-infra/docs/assistant/config-format.md`, version 1). It ignores keys it does not need, as that format allows, and rejects versions it does not know.
- `limits`: the rate limit, a token bucket of texts per caller. `retry`: the retry policy (exponential backoff or the provider's `Retry-After`, within a time limit); both use tokio's clock, so tests run them on paused time.
- `provider`: the `Provider` trait and `Mistral`, which calls `genai` with an explicit `ServiceTarget` (endpoint, key and adapter), so genai never guesses from model names or the environment. Provider errors are transient (retried) or permanent.
- `usage`: the SQLite usage database (`usage` and `refusal` tables, migrations in `crates/llm-gateway/migrations/`).
- `money`: exact amounts in nano-euros and prices per model; `callers`: caller keys, kept as SHA-256 digests.

Tests use a fake `Provider` (`tests/embeddings.rs`) and a fake Mistral HTTP API (`tests/mistral.rs`); one ignored test calls the real API when `LLM_GATEWAY_MISTRAL_API_KEY` is set.

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
