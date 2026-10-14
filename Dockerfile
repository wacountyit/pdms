# syntax=docker/dockerfile:1.7
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
# Single image for server, worker, and CLI. Built with SQLx offline metadata
# (.sqlx/), so no database is needed at build time.

ARG RUST_VERSION=1.98
FROM rust:${RUST_VERSION}-bookworm AS build
WORKDIR /src
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p pdms \
 && install -m 0755 target/release/pdms /pdms

# Minimal Debian runtime from Docker Hub (widely mirrored), running as a
# fixed non-root UID. rustls and embedded tz data mean no extra packages.
FROM debian:bookworm-slim
LABEL org.opencontainers.image.title="PDMS" \
      org.opencontainers.image.description="Policy and Document Management System" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin pdms \
 && install -d -o 10001 -g 10001 -m 0750 /var/lib/pdms /var/lib/pdms/blobs /var/lib/pdms/spool
COPY --from=build /pdms /usr/local/bin/pdms
COPY LICENSE /usr/share/doc/pdms/LICENSE
USER 10001:10001
EXPOSE 8080
ENV PDMS_SERVER__BIND=0.0.0.0:8080 \
    PDMS_STORAGE__LOCAL_PATH=/var/lib/pdms/blobs \
    PDMS_STORAGE__SPOOL_PATH=/var/lib/pdms/spool
# Named volumes mounted here inherit the ownership above on first use.
VOLUME ["/var/lib/pdms"]
HEALTHCHECK --interval=30s --timeout=6s --start-period=20s CMD ["/usr/local/bin/pdms", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/pdms"]
CMD ["serve"]
