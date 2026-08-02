# Runtime image for an already-built save_audio_stream release — nothing is
# compiled here.
#
# The build context is an *extracted release tarball* (see build-tarball.sh), so
# the image ships the exact binary and frontend bundle that were attached to the
# GitHub release: the frontend is built once for every platform, and each arch's
# image reuses that arch's release binary instead of rebuilding it.
# .github/workflows/release.yml wires this up; to do it by hand:
#
#   tar -xzf save_audio_stream-0.2.14-linux-x86_64.tar.gz
#   docker build -f packaging/Dockerfile --build-arg VERSION=0.2.14 \
#     save_audio_stream-0.2.14
#
# The base has to be at least as new as the builder: the release binaries are
# built on the ubuntu-24.04 runners (glibc 2.39), and trixie's 2.41 runs them
# fine — a base with an *older* glibc would not.
FROM debian:trixie-slim

LABEL org.opencontainers.image.source=https://github.com/andrewtheguy/save_audio_stream

# ca-certificates for HTTPS stream sources. libssl3 and zlib1g are named
# explicitly rather than left to arrive transitively: `ldd` on the release
# binary shows libssl.so.3, libcrypto.so.3, libz.so.1 and libzstd.so.1 —
# reqwest's native-tls and ssh2's libssh2 both link OpenSSL. Naming them means a
# change to the dependency chain fails here, not at the first HTTPS request.
#
# tini because save_audio_stream does not install a SIGTERM handler: as PID 1 it
# would inherit the default "ignore" disposition, and `docker stop` would time
# out after 10s and SIGKILL it mid-write. SQLite's WAL wants a cleaner close
# than that.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libssl3 zlib1g libzstd1 tini \
    && rm -rf /var/lib/apt/lists/*

# The version is part of the on-disk layout, not decoration: save_audio_stream
# derives its config and data paths from its own location —
# <prefix>/versions/<version>/bin/save_audio_stream resolves to <prefix>/etc and
# <prefix>/data (see src/paths.rs). Mirroring the real install layout is what
# makes a bare `record` find /opt/save_audio_stream/etc/record.toml.
ARG VERSION
RUN test -n "${VERSION}" || { echo "the VERSION build-arg is required" >&2; exit 1; }

COPY bin/save_audio_stream /opt/save_audio_stream/versions/${VERSION}/bin/save_audio_stream
COPY share /opt/save_audio_stream/versions/${VERSION}/share

# `current` is the stable path the entrypoint uses; canonicalizing it is how the
# binary finds the versioned root, exactly as with the tarball installer.
RUN ln -s "versions/${VERSION}" /opt/save_audio_stream/current \
    && mkdir -p /opt/save_audio_stream/etc /opt/save_audio_stream/data/recordings

# Config comes from outside — bind-mount it read-only, it holds credentials:
#
#   docker run -p 17000:17000 \
#     -v ./record.toml:/opt/save_audio_stream/etc/record.toml:ro \
#     -v ./credentials.toml:/opt/save_audio_stream/etc/credentials.toml:ro \
#     -v sas-data:/opt/save_audio_stream/data \
#     ghcr.io/andrewtheguy/save_audio_stream:vX.Y.Z record
#
# Passwords can come from the environment instead of the credentials file
# (CRED_SFTP_<PROFILE>_PASSWORD, CRED_POSTGRES_<PROFILE>_PASSWORD — see
# src/credentials.rs), which is usually the better answer in a container.
#
# Recordings have to outlive the container, so unlike the config directory this
# one is a declared volume.
VOLUME ["/opt/save_audio_stream/data"]

# 17000 record, 18000 receiver.
EXPOSE 17000 18000

ENTRYPOINT ["/usr/bin/tini", "--", "/opt/save_audio_stream/current/bin/save_audio_stream"]
CMD ["record"]
