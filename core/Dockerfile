# Argus Core (Rust). Migrations are baked in via include_str! at compile time.
FROM rust:1-slim AS build
WORKDIR /app
COPY Cargo.toml ./
COPY core ./core
RUN cargo build --release -p argus-core

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/argus-core /usr/local/bin/argus-core
CMD ["argus-core"]
