FROM rust:1.92.0-alpine3.23@sha256:71571f70b9040894fad9194ab3bc70ca1ccd705e1af979d8d79be74fa7ebcfcd AS builder

ARG GIT_CONFIG_PARAMETERS=""
ARG CARGO_HOME=/usr/src/.cargo/
ARG CARGO_BUILD_FLAGS=""
ARG RUSTFLAGS=""

RUN apk add --no-cache --upgrade build-base openssl-dev openssl-libs-static pkgconf;

WORKDIR /usr/src

COPY ./src ./src
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

RUN --mount=type=cache,target=/usr/src/.cargo/registry \
    --mount=type=cache,target=/usr/src/target \
    cargo build ${CARGO_BUILD_FLAGS} --release;

RUN --mount=type=cache,target=/usr/src/target \
    cp ./target/release/catalog-mcp-server /usr/local/bin/;

##############################################################################################

FROM docker.io/alpine:3.23.0@sha256:51183f2cfa6320055da30872f211093f9ff1d3cf06f39a0bdb212314c5dc7375

COPY --from=builder /usr/local/bin/catalog-mcp-server /usr/local/bin/catalog-mcp-server

RUN chmod +x /usr/local/bin/catalog-mcp-server;

#-- user --#

ARG USERNAME=catalog-mcp-server
ARG USER_UID=1000
ARG USER_GID=$USER_UID

RUN addgroup --gid $USER_GID $USERNAME \
    && adduser --uid $USER_UID --ingroup $USERNAME --system --home /home/$USERNAME $USERNAME;

USER catalog-mcp-server

WORKDIR /home/catalog-mcp-server

ENTRYPOINT ["catalog-mcp-server"]
