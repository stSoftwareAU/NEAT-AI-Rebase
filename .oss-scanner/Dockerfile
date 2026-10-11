# OSS Scanner build image (https://github.com/anthropics/oss-scanner).
# The scanner builds this with the repository as the build context, then runs
# its audit offline, so the toolchain, every crate (including the git
# dependency on NEAT-AI-core) and the test binaries are fetched and built
# here. The base image matches the rust-toolchain.toml pin.
FROM rust:1.99.0-bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates bash git python3 \
    && rm -rf /var/lib/apt/lists/*

# Full debug info and frame pointers. An exported RUSTFLAGS replaces the
# host-local `target-cpu=native` in .cargo/config.toml, as ./quality.sh and CI
# do, so the binaries do not depend on the build VM's CPU.
ENV CARGO_PROFILE_DEV_DEBUG=full \
    RUSTFLAGS="-C force-frame-pointers=yes" \
    RUST_BACKTRACE=1

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Toolchain and components from rust-toolchain.toml, then every crate in
# Cargo.lock, then the binaries and every test target, so the offline
# audit can run `cargo test` and `cargo run` without a network.
RUN rustup toolchain install && rustc --version \
    && cargo fetch --locked \
    && cargo build --workspace --all-targets --all-features --locked \
    && cargo test --workspace --all-features --locked --no-run

# The NEAT-AI-scorer `rust_scorer` binary on PATH, for the real-scorer tests
# and end-to-end runs (its CPU path; the scanner has no GPU). Optional: a
# failure warns rather than aborting the build, and those tests then skip.
RUN (cargo install --locked --git https://github.com/stSoftwareAU/NEAT-AI-scorer \
      rust_scorer --bin rust_scorer \
      || echo "WARNING: rust_scorer install failed; real-scorer tests will skip")

# Smoke-run the library unit tests; a failure warns rather than aborting the
# build. The full suite is `cargo test --workspace --all-features` from /src.
RUN (cargo test --workspace --all-features --locked --lib -- --test-threads=2 \
      || echo "WARNING: smoke tests reported failures")
