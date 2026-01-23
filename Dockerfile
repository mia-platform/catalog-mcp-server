FROM rust:1.93.0-alpine3.23@sha256:d776c22db3cf28689f145615e7deab6ee7496cfc7d6cdefde3fc050d05e4a4dc AS builder

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

FROM docker.io/alpine:3.23.2@sha256:865b95f46d98cf867a156fe4a135ad3fe50d2056aa3f25ed31662dff6da4eb62

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
