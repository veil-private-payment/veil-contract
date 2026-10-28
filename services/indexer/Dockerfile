# syntax=docker/dockerfile:1
#
# Build from the repository root:
#
#   docker build -f services/indexer/Dockerfile -t veil-indexer .
#
# The runtime image holds the binary and nothing else: no shell, no package
# manager, and a non-root user.

FROM rust:1.92.0-slim-bookworm AS build
WORKDIR /src
COPY . .
# rust-toolchain.toml pins 1.92.0, which this image already has. Its wasm
# targets are only needed for the contracts, so skip installing them here.
ENV RUSTUP_TOOLCHAIN=1.92.0
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p veil-indexer \
 && cp target/release/veil-indexer /veil-indexer

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /veil-indexer /usr/local/bin/veil-indexer
USER nonroot:nonroot
EXPOSE 8080
ENV VEIL_LISTEN=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/veil-indexer"]
CMD ["run"]
