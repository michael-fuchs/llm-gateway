# llm-gateway

A small LLM gateway for self-hosted services. Services call it instead of an LLM provider, and it alone holds the provider keys. It owns what every caller would otherwise repeat:

- a monthly budget for all callers together;
- rate limits and retries against the provider;
- usage and estimated cost per calling service and sphere;
- rules about which content may go to which provider.

It is one service of a personal assistant built from independent services (wiki, documents, calendar). Each service keeps its own job queue and calls the gateway for embeddings and, later, other LLM work. The gateway never queues work: it answers or refuses, and callers wait and retry on their side.

Status: batch embeddings from Mistral with usage accounting, a monthly budget, sphere rules, a rate limit per caller and retries against the provider, and a usage view for the owner.

## API

### `POST /v1/embeddings`

Embeds a batch of texts. The caller authenticates with its key as a bearer token and names the sphere the texts belong to.

```http
POST /v1/embeddings
Authorization: Bearer <caller key>
Content-Type: application/json

{"sphere": "work", "model": "mistral-embed", "inputs": ["first text", "second text"]}
```

- `sphere`: one of the spheres in the spheres file (see `--spheres`).
- `model`: one of the configured models (see `--prices`).
- `inputs`: 1 to 512 texts, and no more than the caller's rate limit per minute. The request body may be up to 8 MiB.

The answer has one embedding per input, in the order of the inputs, and what the call used:

```json
{
  "model": "mistral-embed",
  "embeddings": [[0.012, -0.034, ...], [0.021, 0.007, ...]],
  "usage": {"input_tokens": 9, "cost_eur": 0.0000009}
}
```

### Errors

Errors have a JSON body with a machine-readable `code` and a message for people:

```json
{"error": {"code": "budget_exhausted", "message": "the monthly budget of 10.00 EUR is used up until 2026-11-01T00:00:00Z", "retry_at": "2026-11-01T00:00:00Z", "retry_after_seconds": 43200}}
```

| Status | `code`             | Meaning                                                                                                                                                                           |
| ------ | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 400    | `invalid_request`  | The body is not a valid request (missing or unknown fields, no inputs, too many inputs, bad sphere name).                                                                         |
| 400    | `unknown_model`    | The model has no configured price, so the gateway does not offer it.                                                                                                             |
| 400    | `unknown_sphere`   | The sphere is not declared in the spheres file.                                                                                                                                  |
| 401    | `unauthenticated`  | No bearer token, or not the key of a configured caller.                                                                                                                          |
| 403    | `sphere_rule`      | The sphere's rules do not allow its content to go to the provider. The provider was not contacted. Retrying does not help.                                                      |
| 429    | `budget_exhausted` | The monthly budget is used up. Wait until `retry_at` (the start of the next month, UTC); `retry_after_seconds` and the `Retry-After` header give the same in seconds. The provider was not contacted. |
| 429    | `rate_limited`     | The caller sent more texts than its rate limit allows. Wait `retry_after_seconds` (also in the `Retry-After` header). The provider was not contacted.                          |
| 502    | `provider_failure` | The provider refused the call, or failed transiently until the time limit ran out. Nothing was recorded; retry later.                                                          |
| 500    | `internal`         | The gateway's database failed.                                                                                                                                                    |

### `GET /health`

Answers `ok` while the gateway runs.

## Sphere rules, rate limit and retries

A request is checked in this order before anything leaves the server: the request itself, the sphere's rules, the budget, the caller's rate limit.

- **Sphere rules** come from the spheres file, the part of the assistant's central configuration rendered for the gateway (format version 1): a `[spheres.<name>]` table per sphere, whose optional `rules.llm` lists the only providers that may receive the sphere's content. An empty list allows none; without the key every provider is allowed. Other keys, such as descriptions and the wiki rule, are ignored. The file is read at startup.

  ```toml
  version = 1

  [spheres.work]
  rules = { llm = ["mistral"] }

  [spheres.private]
  ```

- **Rate limit:** each caller may send `--inputs-per-minute` texts per minute. It may use a minute's allowance at once; the allowance then refills steadily. It counts texts rather than requests, because texts drive the cost.
- **Retries:** rate limits (429), timeouts (408) and server errors (5xx) from the provider, failures to reach it and attempts without an answer within `--provider-attempt-timeout` are retried. Waits start at 0.5 s and double up to 10 s, or are as long as the provider asks with `Retry-After`. Once the next wait would end after `--provider-time-limit`, the request fails with `provider_failure`. Other errors (a rejected key or request) are not retried.

Refusals for sphere rules, the budget and the rate limit are recorded in the database with their reason (`sphere_rule`, `budget_exhausted`, `rate_limited`).

## Usage and budget

Every answered call is recorded in the SQLite database with its time, caller, sphere, provider, model, number of inputs, input tokens and estimated cost. The cost is the input tokens the provider reports times the configured price of the model.

The budget covers all callers together for a calendar month in UTC. Before each call the gateway adds up the month's costs; once they reach the budget, it refuses every request with `budget_exhausted` until the next month, and records the refusal. Since a call's cost is known only after the call, the call that crosses the budget is still answered, so the budget may be overrun by what the calls in flight at that moment cost.

## Usage view

A small page for the owner shows what LLM use costs, month by month:

- the budget used and what is left;
- calls, texts, tokens and estimated cost per caller, sphere and model, and the cost per day and caller as a chart;
- refusals by reason, caller and sphere;
- earlier months with links to them.

It is server-rendered HTML and CSS without JavaScript, follows the browser's light or dark theme and works at phone width. It is served on its own listener (`--web-listen`), apart from the API, so a reverse proxy can expose the page to the owner's browser while the API stays on the internal network.

Access needs the owner's login through an OIDC provider (authorization code flow with PKCE): only members of the owner group (`--owner-group`, from the ID token's `groups` claim) get a session. Register a client for the view at the provider with the redirect URI `<public-url>/auth/callback` and the scopes `openid email groups`. Without `--oidc-issuer` the view has no login, which is only allowed on a loopback address, for development.

## Configuration

Options are given on the command line or through the environment:

| Option           | Environment                     | Default                     | Meaning                                                                                                                                      |
| ---------------- | ------------------------------- | --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `--db`           | `LLM_GATEWAY_DB`                | `llm-gateway.db`            | Path of the SQLite usage database; created if missing.                                                                                      |
| `--listen`       | `LLM_GATEWAY_LISTEN`            | `127.0.0.1:8080`            | Address to listen on.                                                                                                                        |
| `--callers`      | `LLM_GATEWAY_CALLERS`           | (required)                  | Callers and their keys as `<name>=<key>`, comma-separated, e.g. `mnemograph=<key>,documents=<key>`. Keys must be at least 16 characters and unique. |
| `--mistral-api-key` | `LLM_GATEWAY_MISTRAL_API_KEY` | (required)                  | The Mistral API key.                                                                                                                         |
| `--mistral-url`  | `LLM_GATEWAY_MISTRAL_URL`       | `https://api.mistral.ai/v1/` | Base URL of Mistral's API.                                                                                                                  |
| `--budget-eur`   | `LLM_GATEWAY_BUDGET_EUR`        | `10`                        | Monthly budget for all callers together, in euros.                                                                                          |
| `--prices`       | `LLM_GATEWAY_PRICES`            | `mistral-embed=0.1`         | Offered models and their prices as `<model>=<euros per million input tokens>`, comma-separated. Check them against the provider's price list. |
| `--spheres`      | `LLM_GATEWAY_SPHERES`           | (required; `/etc/assistant/spheres.toml` in the image) | The spheres file with the sphere rules (see above).                                                                    |
| `--inputs-per-minute` | `LLM_GATEWAY_INPUTS_PER_MINUTE` | `1000`                 | Texts each caller may send per minute; also the largest batch.                                                                              |
| `--provider-time-limit` | `LLM_GATEWAY_PROVIDER_TIME_LIMIT` | `60`               | The longest a provider call may take in seconds, retries included.                                                                         |
| `--provider-attempt-timeout` | `LLM_GATEWAY_PROVIDER_ATTEMPT_TIMEOUT` | `30`     | The longest one attempt waits for the provider's answer, in seconds.                                                                       |
| `--web-listen`   | `LLM_GATEWAY_WEB_LISTEN`        | (none: no usage view)       | Address of the usage view, e.g. `0.0.0.0:8081`.                                                                                             |
| `--oidc-issuer`  | `LLM_GATEWAY_OIDC_ISSUER`       | (none: no login)            | Issuer of the owner's ID tokens, exactly as in their `iss` claim. Required unless the view listens on a loopback address.                  |
| `--public-url`   | `LLM_GATEWAY_PUBLIC_URL`        | `http://localhost:8081`     | The URL under which the browser reaches the view; cookies are marked `Secure` when it is `https`.                                         |
| `--web-client-id` | `LLM_GATEWAY_WEB_CLIENT_ID`    | `llm-gateway-web`           | The view's client ID at the OIDC provider.                                                                                                 |
| `--web-client-secret` | `LLM_GATEWAY_WEB_CLIENT_SECRET` | (none)                | The client's secret if it is confidential; a public client relies on PKCE.                                                                 |
| `--owner-group`  | `LLM_GATEWAY_OWNER_GROUP`       | `llm_gateway_owner`         | Only members of this group may log in to the view.                                                                                         |

The caller keys, the Mistral key and the web client's secret are secrets: pass them through the environment from the server's secret handling, not on the command line. Generate a caller key with, for example, `openssl rand -hex 32`. The log level is set with `RUST_LOG` (default `info`).

## Running

```sh
printf 'version = 1\n[spheres.work]\n' > spheres.toml
LLM_GATEWAY_CALLERS=mnemograph=$(openssl rand -hex 32) \
LLM_GATEWAY_MISTRAL_API_KEY=... \
LLM_GATEWAY_SPHERES=spheres.toml \
LLM_GATEWAY_WEB_LISTEN=127.0.0.1:8081 \
cargo run -- serve
```

This serves the API on port 8080 and the usage view without a login on <http://127.0.0.1:8081/>.

The container image (`ghcr.io/michael-fuchs/llm-gateway`) is distroless and runs as a non-root user. It serves the API on port 8080 (and the usage view on 8081 when `LLM_GATEWAY_WEB_LISTEN=0.0.0.0:8081` is set with a login), keeps its database in the volume `/data` and reads the spheres file from `/etc/assistant/spheres.toml`:

```sh
docker run -p 8080:8080 -v llm-gateway:/data \
  -v ./spheres.toml:/etc/assistant/spheres.toml:ro \
  -e LLM_GATEWAY_CALLERS=... -e LLM_GATEWAY_MISTRAL_API_KEY=... \
  ghcr.io/michael-fuchs/llm-gateway:main
```

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test
```

The tests use a fake provider and a fake Mistral API; retry and rate-limit tests run on tokio's paused clock, so they take no real time. One ignored test calls the real Mistral API:

```sh
LLM_GATEWAY_MISTRAL_API_KEY=... cargo test -- --ignored
```

## License

MIT
