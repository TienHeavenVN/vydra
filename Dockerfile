# syntax=docker/dockerfile:1.7
FROM rust:1-slim AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config ca-certificates && rm -rf /var/lib/apt/lists/*
# No Cargo.lock is committed on purpose (see docs/security/PROVIDER_ENGINE_AUDIT.md), so the image
# resolves dependencies at build time; rust-toolchain.toml is not copied because the image's
# toolchain already satisfies `edition = "2024"` / `resolver = "3"`.
COPY Cargo.toml ./
COPY crates ./crates
COPY apps ./apps
COPY migrations ./migrations
RUN cargo build --release -p vydra-api-server -p vydra-worker

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
WORKDIR /app
COPY --from=builder /build/target/release/vydra-api-server /usr/local/bin/vydra-api-server
COPY --from=builder /build/target/release/vydra-worker /usr/local/bin/vydra-worker
COPY migrations /app/migrations
ENV VYDRA__SERVER__HOST=0.0.0.0 \
    VYDRA__SERVER__PORT=8080 \
    VYDRA__OBSERVABILITY__LOG_FORMAT=json
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/vydra-api-server"]
