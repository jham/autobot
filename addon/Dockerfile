# Built by CI with the repository root as the context:
#   docker build -f addon/Dockerfile --build-arg BUILD_ARCH=amd64 .
# Each architecture builds natively on its own runner, so there is no cross-compiling.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked

FROM alpine:3.22
ARG BUILD_ARCH
ARG BUILD_VERSION
LABEL io.hass.type="addon" \
      io.hass.name="autobot" \
      io.hass.arch="${BUILD_ARCH}" \
      io.hass.version="${BUILD_VERSION}" \
      org.opencontainers.image.source="https://github.com/jham/autobot" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /src/target/release/autobot /usr/bin/autobot
ENTRYPOINT ["/usr/bin/autobot"]
# Outside Home Assistant, run with: -c /config/autobot.toml
CMD ["--addon"]
