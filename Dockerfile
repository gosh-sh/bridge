# Multi-stage Dockerfile for Acki Nacki Bridge

# Stage 1: Rust builder
FROM rust:1.94-slim as rust-builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    git \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build dependencies (cached layer)
RUN mkdir -p crates/eth-frontend/src \
    && echo "fn main() {}" > crates/eth-frontend/src/main.rs \
    && cargo build --release \
    && rm -rf crates/*/src

# Copy actual source code
COPY crates ./crates

# Build the project
RUN cargo build --release --workspace

# Stage 2: Foundry builder
FROM ghcr.io/foundry-rs/foundry:latest as foundry-builder

WORKDIR /app

# Copy contract files
COPY contracts/ethereum ./contracts/ethereum

# Build contracts
WORKDIR /app/contracts/ethereum
RUN forge install
RUN forge build

# Stage 3: Runtime
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy built artifacts from builders
COPY --from=rust-builder /app/target/release/eth-frontend /usr/local/bin/
COPY --from=foundry-builder /app/contracts/ethereum/out ./contracts/out

# Copy configuration files
COPY .env.example .env

# Create non-root user
RUN useradd -m -u 1000 bridge && chown -R bridge:bridge /app
USER bridge

# Expose ports (if needed)
EXPOSE 8080

# Default command
CMD ["eth-frontend"]

