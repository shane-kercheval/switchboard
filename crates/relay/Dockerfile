# The relay's container. Build it with `make relay-image`, which passes the
# Rust version from rust-toolchain.toml so the image and the gates build with
# the same toolchain. By hand, from the repository root, which the workspace
# needs:
#
#   docker build -f crates/relay/Dockerfile --build-arg RUST_VERSION=<channel> -t switchboard-relay .
#   docker run --rm -p 8080:8080 switchboard-relay
#
# TLS is the host's job: clients reach the relay as wss://, and the host
# forwards plain HTTP to PORT. Configuration is environment variables
# (crates/relay/src/config.rs).

# Required, and deliberately without a default: a second copy of the version
# here would drift from rust-toolchain.toml.
ARG RUST_VERSION
FROM rust:${RUST_VERSION}-slim-trixie AS build
WORKDIR /src
# Only the Cargo workspace (see Dockerfile.dockerignore). Without the
# repository's rust-toolchain.toml, which adds the iOS targets, the image's
# own toolchain, of the same version, builds the relay.
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --profile relay --locked -p switchboard-relay \
    && cp target/relay/switchboard-relay /usr/local/bin/

FROM debian:trixie-slim
RUN useradd --system --no-create-home relay
COPY --from=build /usr/local/bin/switchboard-relay /usr/local/bin/
USER relay
ENV PORT=8080
EXPOSE 8080
CMD ["switchboard-relay"]
