FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY server server
RUN cargo build --locked --release -p gamesync-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl gosu \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 10001 -g 10001 -m 0700 /var/lib/gamesync
COPY --from=build /src/target/release/gamesync-server /usr/local/bin/gamesync-server
COPY server/docker-entrypoint.sh /usr/local/bin/gamesync-entrypoint
ENV GAMESYNC_DATA=/var/lib/gamesync GAMESYNC_BIND=0.0.0.0 GAMESYNC_PORT=8787
EXPOSE 8787
HEALTHCHECK --interval=15s --timeout=3s --retries=3 CMD curl --fail --silent --max-time 2 "http://127.0.0.1:${GAMESYNC_PORT}/healthz" || exit 1
ENTRYPOINT ["/bin/sh", "/usr/local/bin/gamesync-entrypoint"]
