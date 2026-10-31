# Glossary

- **Caller:** a service that calls the gateway, identified by its own key (e.g. `mnemograph`). Not a person or agent.
- **Sphere:** the area of life the content of a request belongs to (e.g. `work`, `private`), named by the caller. Spheres come from the assistant's central configuration; their rules decide which providers may see their content.
- **Provider:** the LLM service a call goes to (e.g. Mistral). Only the gateway holds provider keys.
- **Model:** a provider's model the gateway offers. Only models with a configured price are offered.
- **Usage:** the record of one answered call: caller, sphere, provider, model, input tokens and estimated cost.
- **Budget:** the most the estimated costs of all callers together may reach in a calendar month (UTC).
- **Refusal:** a request the gateway turned down before contacting the provider for a reason the owner should see (budget exhausted; later rate limited or a sphere rule). Recorded with its reason. Invalid or unauthenticated requests are errors, not refusals.
