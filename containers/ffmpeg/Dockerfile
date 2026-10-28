FROM docker.io/library/debian:13.7-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4 AS packages
# Frozen signed snapshots pin the complete architecture-specific dependency closure.
RUN rm /etc/apt/sources.list.d/debian.sources && \
    printf '%s\n' \
    'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] http://snapshot.debian.org/archive/debian/20261027T000000Z/ trixie main' \
    'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] http://snapshot.debian.org/archive/debian-security/20261027T000000Z/ trixie-security main' \
    > /etc/apt/sources.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates=20250419+deb13u1 && \
    sed -i 's|http://snapshot|https://snapshot|g' /etc/apt/sources.list

FROM packages AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential=12.12 nasm=2.16.03-1 pkgconf=1.8.1-4 patch=2.8-2 \
    gnupg=2.4.7-21+deb13u1 curl=8.14.1-2+deb13u7 xz-utils=5.8.1-1+deb13u2 \
    libx264-dev=2:0.164.3108+git31e19f9-2+b1 libx265-dev=4.1-2 \
    libsvtav1enc-dev=2.3.0+dfsg-1 libopus-dev=1.5.2-2 \
    libaom-dev=3.12.1-1+deb13u1 zlib1g-dev=1:1.3.dfsg+really1.3.1-1+b1
WORKDIR /build
COPY pcm-wavex.patch Dockerfile ./
RUN curl --fail --location --retry 3 --max-time 120 -O https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz && \
    curl --fail --location --retry 3 --max-time 120 -O https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz.asc && \
    curl --fail --location --retry 3 --max-time 120 -O https://ffmpeg.org/ffmpeg-devel.asc && \
    printf '%s\n' \
    '8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e  ffmpeg-9.0.2.tar.xz' \
    'd617fd94ea354dadd2a8bb16243d37c44e1e9729b06b6ad58fe30bfb0fa943f2  ffmpeg-9.0.2.tar.xz.asc' \
    '397b3becedcd5a98769967ff1ff8501ddc89f8368b8f766e4701377d7dbaabe5  ffmpeg-devel.asc' \
    > source-sha256 && sha256sum --check source-sha256 && \
    mkdir -m 700 /build/gnupg && \
    gpg --homedir /build/gnupg --batch --import ffmpeg-devel.asc 2> key-import.log && \
    gpg --homedir /build/gnupg --batch --status-fd 1 --verify \
    ffmpeg-9.0.2.tar.xz.asc ffmpeg-9.0.2.tar.xz > signature-status 2> signature-diagnostics && \
    grep '^\[GNUPG:\] VALIDSIG FCF986EA15E6E293A5644F10B4322F04D67658D8 ' signature-status && \
    tar -xf ffmpeg-9.0.2.tar.xz && \
    patch --batch --fuzz=0 -p1 -d ffmpeg-9.0.2 < pcm-wavex.patch
WORKDIR /build/ffmpeg-9.0.2
RUN ./configure --prefix=/usr/local --enable-gpl \
    --enable-libx264 --enable-libx265 --enable-libsvtav1 --enable-libopus --enable-libaom \
    --disable-autodetect --enable-zlib --enable-iconv --disable-network \
    --disable-ffplay --disable-doc --disable-debug --enable-static --disable-shared && \
    make -j8 && make install && \
    mkdir -p /usr/local/share/voom-ffmpeg && \
    cp /build/ffmpeg-9.0.2.tar.xz /build/ffmpeg-9.0.2.tar.xz.asc \
    /build/ffmpeg-devel.asc /build/pcm-wavex.patch /build/Dockerfile /build/source-sha256 \
    COPYING.GPLv2 /usr/local/share/voom-ffmpeg/ && \
    sha256sum /build/pcm-wavex.patch > /usr/local/share/voom-ffmpeg/patch-sha256 && \
    dpkg-query -W > /usr/local/share/voom-ffmpeg/build-packages && \
    dpkg --print-architecture > /usr/local/share/voom-ffmpeg/architecture && \
    ffmpeg -buildconf > /usr/local/share/voom-ffmpeg/buildconf 2>&1

FROM packages AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    libx264-164=2:0.164.3108+git31e19f9-2+b1 libx265-215=4.1-2 \
    libsvtav1enc2=2.3.0+dfsg-1 libopus0=1.5.2-2 libaom3=3.12.1-1+deb13u1 \
    zlib1g=1:1.3.dfsg+really1.3.1-1+b1 && \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/ffmpeg /usr/local/bin/ffprobe /usr/local/bin/
COPY --from=build /usr/local/share/voom-ffmpeg /usr/local/share/voom-ffmpeg
RUN dpkg-query -W > /usr/local/share/voom-ffmpeg/runtime-packages && \
    ldd /usr/local/bin/ffmpeg && ldd /usr/local/bin/ffprobe && \
    ffmpeg -version && ffprobe -version && \
    ffmpeg -hide_banner -encoders && ffmpeg -hide_banner -muxers && \
    ffmpeg -hide_banner -h muxer=matroska
LABEL org.opencontainers.image.source="https://github.com/randomparity/voom-v2" \
      org.opencontainers.image.version="9.0.2-pcm-wavex" \
      org.opencontainers.image.licenses="GPL-2.0-or-later"
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ffmpeg"]
CMD ["-version"]
