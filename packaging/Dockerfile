# The image `packaging/kubernetes/deployment.yaml` names.
#
# # Why this file exists
#
# `RUN-11`. The manifest declared `image: ghcr.io/ajsinha/sankhya:0.1.0` and there was no
# Dockerfile, Containerfile or compose file anywhere in the repository, so the manifest could
# not be applied by anybody --- including whoever wrote it. `cargo xtask check-package` now
# compares the tag here against the workspace version and fails the build when they diverge,
# because two version numbers in two files that nothing relates is how a manifest comes to
# name an image that was never pushed.
#
# # This image does not meet the declared platform baseline, and that is measured
#
# The declared baseline is `glibc` 2.28 (`BASELINE_GLIBC` in `xtask/src/package.rs`), which
# is RHEL 8 and Debian 10. This builder is Debian 11, whose `glibc` is 2.31, and the binary
# it produces needs **GLIBC_2.30** --- checked by extracting it from the built image and
# reading its version references, not assumed. So an image built from this file will not
# start on the oldest platform the project says it supports.
#
# It is written here rather than left to be discovered because the alternative was worse.
# Debian 10 is end-of-life and its package archive has moved, so a builder pinned to it is a
# build that breaks on a schedule nobody controls; and the honest way to hit 2.28 is a
# cross-toolchain with an old sysroot, which is real work and is not this file. Alpine is not
# a smaller version of this either --- it is `musl`, a different target with a different
# baseline, and switching to it silently would replace the promise rather than keep it.
#
# `cargo xtask check-package` knows this number and fails when the builder here and the
# baseline there drift apart, so the gap stays exactly this size or gets smaller.
#
# # Why the runtime stage is not `scratch`
#
# The server shells out to `df` for the storage-headroom check, and TLS verification needs a
# certificate store. A `scratch` image has neither, so the diagnostic would report that it
# could not measure free space on every run --- which is honest and useless.

FROM rust:1.97-bullseye AS build
WORKDIR /src
COPY . .
# Locked, so an image built today and an image built next month contain the same
# dependencies. A build that silently resolves a newer minor version is a build whose
# artefact cannot be reproduced from its tag.
RUN cargo build --release --locked -p sankhya-server

FROM debian:bullseye-slim AS runtime
# `ca-certificates` for TLS, `coreutils` for the `df` the diagnostic reads. Both named
# rather than inherited: a base image that stops shipping one of them should fail the build
# here rather than produce a server whose disk check silently stops working.
RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates coreutils \
    && rm -rf /var/lib/apt/lists/*

# 65532 is the conventional non-root uid, and it is the one the deployment manifest's
# `runAsUser` names. The two have to agree or the mounted volume is unwritable.
RUN groupadd --gid 65532 sankhya \
    && useradd --uid 65532 --gid 65532 --home-dir /var/lib/sankhya --create-home sankhya

COPY --from=build /src/target/release/sankhya-server /usr/local/bin/sankhya-server

# The warehouse, the data directory and the audit chain all live here, and the manifest
# mounts a volume over it.
RUN mkdir -p /var/lib/sankhya && chown 65532:65532 /var/lib/sankhya
USER 65532:65532
WORKDIR /var/lib/sankhya

# Declared for documentation. Publishing a port is the orchestrator's decision, and the
# manifest makes it; these say what the image listens on so `docker inspect` answers it.
EXPOSE 5433 5434 9464

ENTRYPOINT ["/usr/local/bin/sankhya-server"]
CMD ["start"]
