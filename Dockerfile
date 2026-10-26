# syntax=docker/dockerfile:1
FROM rust:1.99.1-slim-bookworm AS build
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp target/release/fabrium-control-plane /control-plane
RUN mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build --chown=65532:65532 /data /data
COPY --from=build /control-plane /control-plane
ENV BIND_ADDR=0.0.0.0:8080 DATABASE_URL=sqlite:///data/control-plane.db
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/control-plane"]
