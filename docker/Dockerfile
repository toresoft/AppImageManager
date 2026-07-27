# syntax=docker/dockerfile:1

# Build/test images for app-image-manager.
#
# Deliberately source-free: nothing is COPYed in. The working tree is
# bind-mounted at /src at run time, so editing a file on the host is
# immediately visible in the container and no image rebuild is needed for a
# code change. Only the toolchain lives in the image.
#
# The base images are pinned to the same distributions the release workflow
# uses (.github/workflows/release.yml), because that is what fixes the glibc
# each package links against:
#   * ubuntu:22.04 — baseline for tests and the .deb (glibc 2.35, so the
#     binary also runs on 24.04 and later)
#   * fedora:44    — the .rpm must link against Fedora 44's glibc
#
# The Rust toolchain is not pinned (CI tracks stable too); override with
# --build-arg RUST_VERSION=1.90.0 for a reproducible toolchain.

# ─────────────────────────────────────────────────────────────────────────────
# Ubuntu toolchain — fmt, clippy, test, release build.
# ─────────────────────────────────────────────────────────────────────────────
FROM ubuntu:22.04 AS toolchain

ARG RUST_VERSION=stable

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    CARGO_TERM_COLOR=always \
    DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# `--profile minimal` plus the two components CI checks with; anything else
# would only make the image bigger.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal \
            --default-toolchain "${RUST_VERSION}" \
            --component clippy --component rustfmt \
    # The container may run as an arbitrary UID (see docker/run.sh), and cargo
    # writes to CARGO_HOME even for a plain build.
    && chmod -R a+rwX "${RUSTUP_HOME}" "${CARGO_HOME}"

WORKDIR /src
CMD ["bash"]

# ─────────────────────────────────────────────────────────────────────────────
# Same, plus cargo-deb for the Debian/Ubuntu package.
# ─────────────────────────────────────────────────────────────────────────────
FROM toolchain AS deb

RUN cargo install cargo-deb --locked \
    && chmod -R a+rwX "${CARGO_HOME}"

# ─────────────────────────────────────────────────────────────────────────────
# Fedora 44 + cargo-generate-rpm for the .rpm.
#
# Mirrors the CI job: Fedora's `rustup` package provides rustup-init, which is
# the supported way to get a toolchain new enough for edition 2024 inside the
# container.
# ─────────────────────────────────────────────────────────────────────────────
FROM fedora:44 AS rpm

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    CARGO_TERM_COLOR=always

RUN dnf install -y \
        findutils \
        gcc \
        git \
        make \
        pkgconf-pkg-config \
        rpm-build \
        rustup \
    && dnf clean all

RUN rustup-init -y --no-modify-path --profile minimal --default-toolchain stable \
    && cargo install cargo-generate-rpm --locked \
    && chmod -R a+rwX "${RUSTUP_HOME}" "${CARGO_HOME}"

WORKDIR /src
CMD ["bash"]
