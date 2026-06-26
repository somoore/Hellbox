# LambdaDoom capsule: native aarch64 Chocolate Doom, rendered into a headless X
# server (TigerVNC) and streamed to a browser tab. The display is served by
# noVNC/websockify on :6901; H.264 video on :6903, Opus audio on :6902, and a JSON
# keyboard/mouse channel on :6904 give a tab you can SEE, HEAR, and PLAY.
#
# The whole engine is built in the image: SDL2 + SDL2_mixer + SDL2_net + Chocolate
# Doom are compiled from source (Amazon Linux 2023 ships no SDL2), and the shareware
# DOOM1.WAD is fetched at build time. Drop your own IWAD/PWADs in capsule/app/ to
# override; otherwise that directory can be empty.
FROM public.ecr.aws/lambda/microvms:al2023-minimal

# ---- Display + audio + build/runtime deps ----
# Display + unpack tooling + audio.
RUN dnf install -y tigervnc-server xterm xsetroot python3-pip tar gzip xz binutils which procps-ng \
        shadow-utils pulseaudio pulseaudio-utils \
    && dnf clean all
# X11 / GL / audio libs the SDL2 build and the X stack link against.
RUN dnf install -y glib2 libX11 libXext libXrender libXrandr libXcursor libXfixes \
        libXi libXcomposite libXinerama libxcb libxkbcommon libxkbcommon-x11 \
        libXScrnSaver freetype fontconfig libjpeg-turbo \
    && dnf clean all
RUN dnf install -y alsa-lib libusbx systemd-libs gnutls mesa-libGL mesa-libGLU \
        mesa-dri-drivers pulseaudio-libs ncurses-libs krb5-libs || true
RUN pip3 install --no-cache-dir websockify websockets python-xlib

# ---- ffmpeg with libopus (Lever 2: Opus audio = ~13-15x less egress than PCM) ----
# AL2023 has no ffmpeg package; the BtbN linuxarm64-gpl static build ships ffmpeg
# WITH libopus (--enable-libopus). audio_ws.py pipes parec -> ffmpeg(libopus/ogg).
# Confirm the encoder exists at BUILD time so a broken fetch fails the build.
RUN set -e; \
    if dnf install -y ffmpeg >/dev/null 2>&1 && command -v ffmpeg >/dev/null 2>&1; then \
        echo "[build] ffmpeg from dnf"; \
    else \
        echo "[build] dnf lacks ffmpeg -> BtbN static aarch64 (has libopus)"; \
        cd /tmp; \
        curl -fsSL --retry 3 --max-time 240 -o btbn.tar.xz \
          "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz"; \
        mkdir -p /opt/ff && tar -xJf btbn.tar.xz -C /opt/ff --strip-components=1; \
        install -m 0755 /opt/ff/bin/ffmpeg /usr/local/bin/ffmpeg; \
        rm -rf /tmp/btbn.tar.xz /opt/ff; \
    fi; \
    ffmpeg -version | head -1; \
    ffmpeg -hide_banner -encoders 2>/dev/null | grep -q libopus || { echo "FATAL: ffmpeg lacks libopus encoder"; exit 1; }

# Non-root user: runs BOTH PulseAudio (user mode, avoids system-bus issues) AND the
# game, so they share the PulseAudio user session and the game's audio reaches the sink.
RUN useradd -m -u 1000 app

# noVNC web client.
RUN curl -fsSL https://github.com/novnc/noVNC/archive/refs/tags/v1.5.0.tar.gz \
      | tar -xz -C /opt && ln -s /opt/noVNC-1.5.0 /opt/novnc

# ---- Native aarch64 Chocolate Doom (SDL2) -- video + AUDIO + input ----
# A native ARM build runs directly on the CPU with no translation layer, so it renders
# reliably across the fleet, and SDL2 gives real audio (SDL2_mixer -> PulseAudio) and
# keyboard input.
#
# AL2023 has NO SDL2 in its repos (no EPEL), so the whole SDL2 stack is built from
# source. The toolchain + X11 headers are available on the base image. REQUIRED devel:
# X11 + Xext (video) and pulseaudio-libs (audio -> the capsule null-sink). The rest are
# best-effort X extensions SDL auto-disables.
RUN dnf install -y gcc make autoconf automake libtool pkgconfig \
        libX11-devel libXext-devel pulseaudio-libs-devel alsa-lib-devel \
    && dnf clean all
RUN for p in libXcursor-devel libXi-devel libXrandr-devel libXfixes-devel \
        libXScrnSaver-devel libxkbcommon-devel; do \
      dnf install -y "$p" || echo "[build] optional dep $p unavailable, skipping"; \
    done; dnf clean all || true
ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:/usr/lib64/pkgconfig

# SDL2 from source: X11 video + PulseAudio/ALSA audio, no GL/Wayland/Vulkan (DOOM
# is software-rendered, so we skip GL and its mesa-devel deps).
WORKDIR /tmp/sdl
RUN curl -fsSL -o sdl.tgz https://github.com/libsdl-org/SDL/releases/download/release-2.30.9/SDL2-2.30.9.tar.gz \
    && tar -xzf sdl.tgz --strip-components=1 \
    && ./configure --disable-video-wayland --disable-video-opengl \
        --disable-video-opengles --disable-video-vulkan \
        --enable-video-x11 --enable-pulseaudio --enable-alsa --disable-sdltest \
    && make -j"$(nproc)" && make install \
    && echo "/usr/local/lib" > /etc/ld.so.conf.d/local.conf && ldconfig \
    && rm -rf /tmp/sdl
WORKDIR /

# SDL2_mixer (digital SFX = WAV chunks from the WAD; music codecs disabled to avoid
# extra deps) and SDL2_net (a hard chocolate-doom dep), both against our source SDL2.
WORKDIR /tmp/mixer
RUN curl -fsSL -o mixer.tgz https://github.com/libsdl-org/SDL_mixer/releases/download/release-2.8.0/SDL2_mixer-2.8.0.tar.gz \
    && tar -xzf mixer.tgz --strip-components=1 \
    && ./configure --disable-music-mod-mikmod --disable-music-midi-fluidsynth \
        --disable-music-ogg --disable-music-flac --disable-music-mp3-mpg123 \
        --disable-music-opus --disable-sdltest \
    && make -j"$(nproc)" && make install && ldconfig && rm -rf /tmp/mixer
WORKDIR /tmp/net
RUN curl -fsSL -o net.tgz https://github.com/libsdl-org/SDL_net/releases/download/release-2.2.0/SDL2_net-2.2.0.tar.gz \
    && tar -xzf net.tgz --strip-components=1 \
    && ./configure --disable-sdltest && make -j"$(nproc)" && make install && ldconfig \
    && rm -rf /tmp/net
WORKDIR /

# Chocolate Doom against the source SDL2 stack (pkg-config finds it via PKG_CONFIG_PATH).
WORKDIR /tmp/cdoom
RUN curl -fsSL -o cdoom.tgz https://github.com/chocolate-doom/chocolate-doom/archive/refs/tags/chocolate-doom-3.0.1.tar.gz \
    && tar -xzf cdoom.tgz --strip-components=1 \
    && ./autogen.sh \
    && ./configure --disable-sdltest CFLAGS="-O2 -fcommon" \
    && make -j"$(nproc)" \
    && install -m 0755 src/chocolate-doom /usr/local/bin/chocolate-doom \
    && echo "native chocolate-doom at:" && ls -la /usr/local/bin/chocolate-doom \
    && rm -rf /tmp/cdoom
WORKDIR /

# ---- The IWAD: fetch the freely-redistributable shareware DOOM1.WAD at build time ----
# This is the game data Chocolate Doom loads (-iwad). It is fetched here, never committed
# to the repo, so `doom build` is self-serve. capsule/app/ is also COPYed in first, so you
# can drop your own IWAD/PWADs there to override or extend (run_app.sh loads the first
# *.wad it finds under /home/app/app). The size + IWAD magic are checked so a bad fetch
# fails the build loudly instead of shipping a broken image.
COPY app/ /home/app/app/
RUN set -e; \
    if ! ls /home/app/app/*.wad /home/app/app/*.WAD >/dev/null 2>&1; then \
        echo "[build] fetching shareware DOOM1.WAD"; \
        curl -fsSL --retry 3 -o /home/app/app/DOOM1.WAD \
          https://raw.githubusercontent.com/nneonneo/universal-doom/main/DOOM1.WAD; \
    fi; \
    wad="$(ls /home/app/app/*.wad /home/app/app/*.WAD 2>/dev/null | head -1)"; \
    [ -n "$wad" ] || { echo "FATAL: no IWAD present"; exit 1; }; \
    sz=$(stat -c%s "$wad"); [ "$sz" -ge 4000000 ] || { echo "FATAL: IWAD too small ($sz)"; exit 1; }; \
    head -c 4 "$wad" | grep -q IWAD || { echo "FATAL: $wad is not an IWAD"; exit 1; }; \
    chown -R app:app /home/app/app; \
    echo "== staged app files =="; ls -l /home/app/app

# Combined page served by websockify at / (noVNC iframe + Web Audio player).
COPY index.html /opt/novnc/index.html

# Supervisor + app runner + audio server (rootfs overlay -> /opt/capsule/...).
COPY rootfs/ /
RUN chmod +x /opt/capsule/start.sh /opt/capsule/run_app.sh

EXPOSE 6901 6902 9000
CMD ["/opt/capsule/start.sh"]
