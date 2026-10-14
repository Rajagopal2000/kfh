# Build: the kfh binary, including the embedded web UI.
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p kfh && cp target/release/kfh /kfh

# Run: glibc and libgcc only, no shell, non-root (uid 65532).
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /kfh /usr/local/bin/kfh
EXPOSE 7878
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/kfh"]
CMD ["serve", "--db", "/data/functions.db", "--addr", "0.0.0.0:7878"]
