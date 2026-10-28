# syntax=docker/dockerfile:1
ARG RUST_IMAGE
ARG RUNTIME_IMAGE
FROM ${RUST_IMAGE} AS builder
ARG BUILD_JOBS=2
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY crates crates
COPY config config
COPY vendor/litellm vendor/litellm
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --locked --release --jobs "${BUILD_JOBS}" -p llm-unify-proxy \
    && install -D /build/target/release/llm-unify-proxy /out/llm-unify-proxy

FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.title="LLM Unify" \
      org.opencontainers.image.source="https://github.com/T3pp31/llm-unify" \
      org.opencontainers.image.licenses="MIT"
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/llm-unify-proxy /usr/local/bin/llm-unify-proxy
COPY LICENSE THIRD_PARTY_NOTICES.md /usr/share/licenses/llm-unify/
COPY vendor/litellm/LICENSE /usr/share/licenses/litellm/LICENSE
USER 65532:65532
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/llm-unify-proxy"]
