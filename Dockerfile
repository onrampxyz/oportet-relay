FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

LABEL org.opencontainers.image.source=https://github.com/ithacaxyz/relay

# Builds a cargo-chef plan
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

# Build profile, maxperf by default
ARG BUILD_PROFILE=maxperf
ENV BUILD_PROFILE $BUILD_PROFILE

# Extra Cargo features
ARG FEATURES=""
ENV FEATURES $FEATURES

# Build the sqlx query! macros against the checked-in .sqlx offline data instead
# of a live database (our gas-sponsorship queries are compile-time verified).
ENV SQLX_OFFLINE=true

# Install system dependencies
RUN apt-get update && apt-get -y upgrade && apt-get install -y libclang-dev pkg-config

# Builds dependencies
RUN cargo chef cook --profile $BUILD_PROFILE --recipe-path recipe.json

# Copy source
COPY . .

# Add migrations
ADD migrations migrations

# Build application
RUN cargo build --profile $BUILD_PROFILE --features "$FEATURES" --locked --bin relay

# ARG is not resolved in COPY so we have to hack around it by copying the
# binary to a temporary location
RUN cp /app/target/$BUILD_PROFILE/relay /app/relay

# Use Ubuntu as the release image
FROM ubuntu AS runtime
WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get -y upgrade && apt-get install -y ca-certificates && update-ca-certificates

# Copy relay over from the build stage
COPY --from=builder /app/relay /usr/local/bin

# Bake our Railway config (chains, contracts, sponsorship policy, JWKS url — NO
# secrets). Railway has no file mounts, so the config ships in the image; secrets
# (RELAY_MNEMONIC, RELAY_FUNDER_SIGNER_KEY, RELAY_DB_URL, …) come from the env at
# boot via --config-only.
COPY deploy/railway/relay.yaml /app/relay.yaml

EXPOSE 9119

# Railway runs the image ENTRYPOINT but drops the Dockerfile CMD when the service
# has no explicit start command, so the relay booted with zero args and crashed
# on the required --orchestrator/... flags. Fold the config args into ENTRYPOINT
# so the image always boots in --config-only mode regardless of platform.
ENTRYPOINT ["/usr/local/bin/relay", "--config", "/app/relay.yaml", "--config-only"]
