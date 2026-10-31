# syntax=docker/dockerfile:1

# glibc on purpose: bundled SQLite and rustls build fine on it, and distroless/cc
# brings the CA certificates needed to reach the provider.
FROM rust:1-trixie@sha256:53766005847cbe02c3f59bfa49c1c4bb16695f2c9e5829c3d576a15406f5e733 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p llm-gateway \
    && install -D target/release/llm-gateway /out/llm-gateway \
    && strip /out/llm-gateway \
    && install -d -m 700 -o 65532 -g 65532 /out/data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e
COPY --from=build /out/llm-gateway /usr/local/bin/llm-gateway
COPY --from=build --chown=65532:65532 /out/data /data
ENV LLM_GATEWAY_DB=/data/llm-gateway.db \
    LLM_GATEWAY_SPHERES=/etc/assistant/spheres.toml \
    LLM_GATEWAY_LISTEN=0.0.0.0:8080
VOLUME /data
WORKDIR /data
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/llm-gateway"]
CMD ["serve"]
