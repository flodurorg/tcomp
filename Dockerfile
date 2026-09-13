FROM rust:1.98-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p tcomp-relay

FROM debian:trixie-slim
RUN useradd --system --uid 10001 --create-home tcomp
WORKDIR /app
COPY --from=build /src/target/release/tcomp-relay /usr/local/bin/tcomp-relay
COPY web ./web
USER tcomp
ENV TCOMP_BIND=0.0.0.0:8080 \
    TCOMP_WEB_DIR=/app/web
EXPOSE 8080
ENTRYPOINT ["tcomp-relay"]
