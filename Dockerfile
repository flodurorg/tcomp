FROM rust:1.98-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
RUN cargo build --release --locked -p tcomp

FROM debian:trixie-slim
RUN useradd --system --uid 10001 --create-home tcomp
WORKDIR /app
COPY --from=build /src/target/release/tcomp /usr/local/bin/tcomp
COPY web ./web
USER tcomp
ENV TCOMP_BIND=0.0.0.0:8080 \
    TCOMP_WEB_DIR=/app/web
EXPOSE 8080
ENTRYPOINT ["tcomp", "serve"]
