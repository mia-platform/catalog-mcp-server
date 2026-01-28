FROM rust:1.92.0-alpine3.23@sha256:f6c22e0a256c05d44fca23bf530120b5d4a6249a393734884281ca80782329bc AS builder

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

FROM docker.io/alpine:3.23.3@sha256:25109184c71bdad752c8312a8623239686a9a2071e8825f20acb8f2198c3f659

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
