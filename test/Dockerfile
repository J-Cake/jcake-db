FROM rust:latest AS build

RUN apt-get update && rustup default nightly

COPY ./format /app/format
COPY ./src /app/src
COPY ./util /app/util
COPY ./Cargo.toml /app/Cargo.toml
COPY ./Cargo.lock /app/Cargo.lock

WORKDIR /app
RUN --mount=type=cache,target=/usr/local/cargo/registry \
	--mount=type=cache,target=/usr/local/cargo/git \
	--mount=type=cache,target=/app/target \
    cargo build -Zunstable-options --features web --release --workspace --artifact-dir /opt/jcake-db

FROM debian:trixie-slim AS run
LABEL authors="jcake"

RUN apt-get update

COPY --from=build /opt/jcake-db/* /usr/local/bin

ENTRYPOINT ["/usr/local/bin/web"]