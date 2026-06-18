# Argus Core (Rust). Migrations are baked in via include_str! at compile time.
FROM rust:1-slim AS build
WORKDIR /app
# jito-sdk-rust v0.3.2 pulls reqwest 0.11 with native-tls (it exposes no rustls feature),
# so the Linux build needs system OpenSSL headers. Our own reqwest is rustls-tls; this
# satisfies only the transitive SDK. (macOS native-tls = Secure Transport, so local builds
# never hit this — it surfaces only in this slim Linux image.)
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml ./
COPY core ./core
RUN cargo build --release -p argus-core

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/argus-core /usr/local/bin/argus-core
CMD ["argus-core"]
