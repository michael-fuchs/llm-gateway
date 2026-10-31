# AGENTS.md — llm-gateway

A small Rust service between self-hosted services and LLM providers (Mistral first). It holds the provider keys and enforces the budget, rate limits, retries, usage accounting per service and sphere, and the spheres' rules about providers.

## Status

Planned, no code yet. The work is in the GitHub issues. The surrounding architecture (one service per kind of thing, spheres, a central configuration) is documented privately with the server's infrastructure in `../server-infra/docs/assistant/` (ADR 0003 there is this gateway's reason to exist).

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
