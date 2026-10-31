# llm-gateway

A small LLM gateway for self-hosted services. Services call it instead of an LLM provider, and it alone holds the provider keys. It owns what every caller would otherwise repeat:

- a monthly budget for all callers together;
- rate limits and retries against the provider;
- usage and estimated cost per calling service and sphere;
- rules about which content may go to which provider.

It is one service of a personal assistant built from independent services (wiki, documents, calendar). Each service keeps its own job queue and calls the gateway for embeddings and, later, other LLM work.

Status: planned; see the issues.

## License

MIT
