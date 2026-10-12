# syntax=docker/dockerfile:1
#
# Headless ruro image: the `ruro` CLI/daemon and the standard `ruro-host`.
# The studio is a desktop application and is not included.
#
#   docker build -t ruro .
#   docker run --rm --shm-size=512m -p 7447:7447 ruro
#
# Cross-process wires use iceoryx2 shared memory in /dev/shm. Docker's default
# /dev/shm is 64 MB, which is too small for image pipelines: raise it with
# --shm-size, or use --ipc=host to share shared memory with ruro processes
# outside the container. To deploy a graph from the host:
#
#   ruro --connect tcp/127.0.0.1:7447 run examples/camera.yaml
#
# Real-time process groups additionally need
#   --cap-add=SYS_NICE --ulimit rtprio=95 --ulimit memlock=-1

ARG RUST_VERSION=1.95
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS build
# bindgen is a build dependency of iceoryx2; libclang keeps it usable on every target.
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ruro-cli -p ruro-host \
    && install -Dm755 target/release/ruro /out/ruro \
    && install -Dm755 target/release/ruro-host /out/ruro-host

FROM debian:${DEBIAN_RELEASE}-slim
ARG UID=10001
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid "${UID}" --user-group --home-dir /var/lib/ruro --create-home ruro
COPY --from=build /out/ruro /out/ruro-host /usr/bin/
COPY examples/ /usr/share/ruro/examples/
USER ruro
WORKDIR /var/lib/ruro
ENV RURO_LOG=info,zenoh=warn
EXPOSE 7447
ENTRYPOINT ["/usr/bin/tini", "--", "ruro"]
CMD ["daemon", "--listen", "tcp/0.0.0.0:7447"]
