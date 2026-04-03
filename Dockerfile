# Build stage
FROM rust:1.94-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/updown /usr/local/bin/updown

# Create data directory
RUN mkdir -p /data

# Web portal
EXPOSE 8080
# UDP data channel
EXPOSE 9000/udp

ENV RUST_LOG=info

ENTRYPOINT ["updown"]
CMD ["server", "--bind", "0.0.0.0:8080", "--storage", "/data", "--data-port", "9000"]
