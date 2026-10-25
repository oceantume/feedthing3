# syntax=docker/dockerfile:1

# Build stage. The cache mounts keep crates and compiled dependencies between
# builds, so a deploy only recompiles our own code.
FROM rust:1.99-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/feedthing /feedthing
RUN mkdir /data

# Runtime: the binary and the static files it serves from `static/`, on a
# distroless base: glibc, libgcc, CA certificates (outgoing TLS reads the
# system trust store) and the time zone database (TZ), no shell. Migrations
# are compiled into the binary.
FROM gcr.io/distroless/cc-debian13:nonroot
WORKDIR /app
COPY --from=build /feedthing /app/feedthing
COPY static /app/static
# Owned by nonroot so the volume mounted here starts out writable.
COPY --from=build --chown=nonroot:nonroot /data /data
ENV BIND=0.0.0.0:8080
EXPOSE 8080
CMD ["/app/feedthing"]
