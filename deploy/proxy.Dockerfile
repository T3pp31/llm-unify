ARG RUST_IMAGE
ARG RUNTIME_IMAGE
FROM ${RUST_IMAGE} AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY crates crates
COPY config config
COPY vendor/litellm vendor/litellm
RUN cargo build --locked --release -p llm-unify-proxy

FROM ${RUNTIME_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/llm-unify-proxy /usr/local/bin/llm-unify-proxy
COPY vendor/litellm/LICENSE /usr/share/licenses/litellm/LICENSE
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/llm-unify-proxy"]
