#!/usr/bin/env bash
# Build and test app-image-manager in a container, with plain podman or docker.
#
# This machine has podman but no compose implementation, so this script drives
# the engine directly instead of going through docker/compose.yaml. The two are
# kept equivalent: same images, same volumes, same commands.
#
# Usage:  docker/run.sh <command>
#
#   test    fmt --check, clippy, cargo test   (the CI "build" job)
#   fmt     rewrite the sources with rustfmt
#   build   release binary            -> dist/app-image-manager
#   deb     Debian/Ubuntu package     -> dist/*.deb
#   rpm     Fedora 44 package         -> dist/*.rpm
#   ci      test + build + deb + rpm
#   shell   interactive shell in the Ubuntu toolchain image
#   clean   delete the cached cargo registry and target volumes
#
# Environment:
#   AIM_ENGINE   podman | docker            (default: whichever is installed)
#   AIM_CPUS     CPU limit for the container (default: half the cores)
#   RUST_VERSION toolchain to bake in       (default: stable)

set -euo pipefail

readonly PROJECT=app-image-manager
readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── engine ───────────────────────────────────────────────────────────────────
ENGINE="${AIM_ENGINE:-}"
if [[ -z "$ENGINE" ]]; then
    if command -v podman >/dev/null 2>&1; then
        ENGINE=podman
    elif command -v docker >/dev/null 2>&1; then
        ENGINE=docker
    else
        echo "error: neither podman nor docker found" >&2
        exit 1
    fi
fi

# Rootless podman already maps the container's root to the invoking user, so
# files written into the bind mount are owned correctly. Rootful docker does
# not, and would leave root-owned files in dist/ — run as ourselves there.
USER_ARGS=()
if [[ "$ENGINE" == docker ]]; then
    USER_ARGS=(--user "$(id -u):$(id -g)")
fi

# Compiling is the CPU-hungry part. Default to half the cores so a build never
# makes the desktop unusable; raise it with AIM_CPUS=… for a faster run.
if [[ -z "${AIM_CPUS:-}" ]]; then
    AIM_CPUS=$(( $(nproc) / 2 ))
    (( AIM_CPUS < 1 )) && AIM_CPUS=1
fi

image_for() { echo "localhost/${PROJECT}-$1:latest"; }

# ── build an image stage on demand ───────────────────────────────────────────
ensure_image() {
    local stage="$1" image
    image="$(image_for "$stage")"
    if "$ENGINE" image exists "$image" 2>/dev/null || \
       "$ENGINE" image inspect "$image" >/dev/null 2>&1; then
        return
    fi
    echo "==> building image $image (stage: $stage)" >&2
    # `build` has no --cpus, so express the same limit as a cfs quota.
    # (Not --cpuset-cpus: rootless podman does not get the `cpuset` controller
    # delegated to its user slice, while `cpu` is.) Installing cargo-deb /
    # cargo-generate-rpm compiles a fair amount of code and would otherwise
    # take the whole machine.
    "$ENGINE" build \
        --cpu-period 100000 --cpu-quota "$(( AIM_CPUS * 100000 ))" \
        --target "$stage" \
        --build-arg "RUST_VERSION=${RUST_VERSION:-stable}" \
        -f "$ROOT/docker/Dockerfile" \
        -t "$image" \
        "$ROOT"
}

# run <stage> <target-volume> <registry-volume> <command...>
run() {
    local stage="$1" target_vol="$2" registry_vol="$3"
    shift 3
    ensure_image "$stage"
    # Only allocate a TTY when there is one; `-it` breaks in CI and pipelines.
    local tty_args=()
    [[ -t 0 ]] && tty_args=(-it)
    "$ENGINE" run --rm \
        "${tty_args[@]}" \
        "${USER_ARGS[@]}" \
        --cpus "$AIM_CPUS" \
        --workdir /src \
        --env RUSTFLAGS="-D warnings" \
        --env CARGO_TERM_COLOR=always \
        -v "$ROOT:/src:z" \
        -v "${PROJECT}-${registry_vol}:/usr/local/cargo/registry" \
        -v "${PROJECT}-${target_vol}:/src/target" \
        "$(image_for "$stage")" \
        "$@"
}

# Shorthands for the two toolchains.
ubuntu() { run toolchain target-ubuntu cargo-registry bash -eux -c "$1"; }
fedora() { run rpm target-fedora cargo-registry-fedora bash -eux -c "$1"; }

cmd_test() {
    ubuntu '
        cargo fmt --all -- --check
        cargo clippy --all-targets
        cargo test --all
    '
}

cmd_fmt() {
    # rustfmt rewrites files in the bind mount, so this edits the working tree.
    ubuntu 'cargo fmt --all'
}

cmd_build() {
    ubuntu '
        cargo build --release --locked
        ./target/release/app-image-manager --version
        mkdir -p dist && cp target/release/app-image-manager dist/
    '
}

cmd_deb() {
    run deb target-ubuntu cargo-registry bash -eux -c '
        cargo deb
        mkdir -p dist && cp target/debian/*.deb dist/
        dpkg-deb -c dist/*.deb
    '
}

cmd_rpm() {
    fedora '
        cargo build --release --locked
        cargo generate-rpm
        mkdir -p dist && cp target/generate-rpm/*.rpm dist/
        rpm -qpl dist/*.rpm
    '
}

cmd_ci() {
    cmd_test
    cmd_build
    cmd_deb
    cmd_rpm
    echo "==> artifacts in dist/:"
    ls -la "$ROOT/dist"
}

cmd_shell() {
    run toolchain target-ubuntu cargo-registry bash
}

cmd_clean() {
    for vol in target-ubuntu target-fedora cargo-registry cargo-registry-fedora; do
        "$ENGINE" volume rm -f "${PROJECT}-${vol}" 2>/dev/null || true
    done
    echo "cache volumes removed (images kept; remove them with:"
    echo "  $ENGINE rmi $(image_for toolchain) $(image_for deb) $(image_for rpm))"
}

case "${1:-}" in
    test)  cmd_test ;;
    fmt)   cmd_fmt ;;
    build) cmd_build ;;
    deb)   cmd_deb ;;
    rpm)   cmd_rpm ;;
    ci)    cmd_ci ;;
    shell) cmd_shell ;;
    clean) cmd_clean ;;
    *)
        sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
        exit 1
        ;;
esac
