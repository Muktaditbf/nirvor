# Multi-stage build: dashboard → Rust binary (dashboard embedded) → small distroless runtime.
# Build from the repository root:  docker build -f deploy/Dockerfile -t nirvor .

FROM node:26.13.0-alpine3.24 AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1.99.1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY --from=web /src/web/dist web/dist
RUN cargo build --release --locked -p nirvor && mkdir -p /out/data && cp target/release/nirvor /out/nirvor

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /out/nirvor /usr/local/bin/nirvor
COPY --chown=65532:65532 --from=build /out/data /var/lib/nirvor
COPY --chown=65532:65532 deploy/config.docker.toml /etc/nirvor/config.toml
VOLUME ["/var/lib/nirvor"]
# Docker lets unprivileged users bind low ports inside containers (net.ipv4.ip_unprivileged_port_start=0).
EXPOSE 53/udp 53/tcp 8053/tcp
USER nonroot
ENTRYPOINT ["/usr/local/bin/nirvor", "--json-logs", "-c", "/etc/nirvor/config.toml"]
