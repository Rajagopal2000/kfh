# Build: the kfh binary, including the embedded web UI.
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p kfh && cp target/release/kfh /kfh

# Database builder (target "builder"): kfh plus git and flock for
# deploy/build-db.sh. Kept apart from the server image, which has no shell.
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates util-linux \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /kfh /usr/local/bin/kfh
COPY deploy/build-db.sh /usr/local/bin/build-db
ENV HOME=/tmp
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/build-db"]

# Server (default target): glibc and libgcc only, no shell, non-root (uid 65532).
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /kfh /usr/local/bin/kfh
EXPOSE 7878
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/kfh"]
CMD ["serve", "--db", "/work/live/functions.db", "--addr", "0.0.0.0:7878"]
