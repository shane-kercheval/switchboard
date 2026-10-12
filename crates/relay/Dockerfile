# The relay's container. Build from the repository root, which the workspace
# needs:
#
#   docker build -f crates/relay/Dockerfile -t switchboard-relay .
#   docker run --rm -p 8080:8080 switchboard-relay
#
# TLS is the host's job: clients reach the relay as wss://, and the host
# forwards plain HTTP to PORT. Configuration is environment variables
# (crates/relay/src/config.rs).

FROM rust:1.95-slim-trixie AS build
WORKDIR /src
# Only the Cargo workspace (see Dockerfile.dockerignore). Without the
# repository's rust-toolchain.toml, which adds the iOS targets, the image's
# own 1.95 toolchain builds the relay.
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p switchboard-relay \
    && cp target/release/switchboard-relay /usr/local/bin/

FROM debian:trixie-slim
RUN useradd --system --no-create-home relay
COPY --from=build /usr/local/bin/switchboard-relay /usr/local/bin/
USER relay
ENV PORT=8080
EXPOSE 8080
CMD ["switchboard-relay"]
