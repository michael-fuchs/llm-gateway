# Glossary

- **Caller:** a service that calls the gateway, identified by its own key (e.g. `mnemograph`). Not a person or agent.
- **Sphere:** the area of life the content of a request belongs to (e.g. `work`, `private`), named by the caller. Spheres come from the assistant's central configuration.
- **Sphere rule:** a sphere's restriction on where its content may go; for the gateway, the list of providers allowed to receive it (`rules.llm`).
- **Rate limit:** how many texts a caller may send per minute, protecting the budget from a runaway caller.
- **Provider:** the LLM service a call goes to (e.g. Mistral). Only the gateway holds provider keys.
- **Model:** a provider's model the gateway offers. Only models with a configured price are offered.
- **Usage:** the record of one answered call: caller, sphere, provider, model, input tokens and estimated cost.
- **Budget:** the most the estimated costs of all callers together may reach in a calendar month (UTC).
- **Refusal:** a request the gateway turned down before contacting the provider for a reason the owner should see: `sphere_rule`, `budget_exhausted` or `rate_limited`. Recorded with its reason.
- **Owner:** the person whose assistant this is; the only one who may see the usage view, as a member of the owner group at the OIDC provider.
- **Usage view:** the owner's page showing the budget, usage and refusals per month.
- **Transient / permanent failure:** a provider failure that may go away (rate limited, overloaded, no answer), which the gateway retries, versus one that repeats on every try (rejected key or request), which it reports at once. Invalid or unauthenticated requests are errors, not refusals.
