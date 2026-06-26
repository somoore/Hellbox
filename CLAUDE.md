# CLAUDE.md

Operating guide for Claude Code working in this repo. Read this first.

## What this is

**LambdaDoom** is a DOOM-only demo built on AWS Lambda MicroVMs: **native aarch64
Chocolate Doom** (GPLv2) running the **shareware `DOOM1.WAD`**, wrapped into a Lambda
MicroVM, streamed to a **browser tab** with **video + audio + keyboard input**, and
**suspended/resumed** on a live memory snapshot. It is a fork of the `shrink-wrap`
spike; the binary, SDK usage, proxy, and lifecycle all come from there.

Public repo: `github.com/somoore/LambdaDoom`. See [docs/architecture.md](docs/architecture.md)
and [docs/security.md](docs/security.md).

The headline UX: `ldoom open --name <X>` → DOOM appears in a browser tab.

> **Binary & config:** the binary is **`ldoom`** (Cargo package `lambdadoom`); config +
> state live at **`~/.lambdadoom/`**; infra is the CloudFormation stack
> **`deploy/doom.yaml`** (S3 + build/exec roles). The README, docs/architecture.md,
> docs/security.md, and docs/microvm-ground-truth.md track this reality.

**Proven live** (account `test_AccountA`, in `us-east-1`, `us-east-2`, AND `us-west-2`):

- `aws-sdk-lambdamicrovms` signs (SigV4) and drives the real control plane.
- Full lifecycle: `build → up → open → suspend → resume → down` (+ `ps`).
- Native Chocolate Doom streamed and **playable** (user-confirmed: "sound and controls
  working perfectly"): H.264 video, Opus audio, XTEST keyboard input.
- **Multi-region:** the identical capsule renders + has audio in all three regions,
  across the Graviton fleet.

**Keeper images** (built, CREATED; instances were terminated to stop billing — relaunch
with `up`): `doomnative6` (us-east-1), `choco-ue2` (us-east-2), `choco-uw2` (us-west-2).

## Native ARM (design constraint)

DOOM is a **native aarch64** build (Chocolate Doom). The capsule runs directly on the ARM
CPU with no translation layer, which is what keeps rendering reliable on every host and
region. Keep it that way: anything added to the capsule should be native ARM.

## The capsule (`capsule/`)

- **Base:** `public.ecr.aws/lambda/microvms:al2023-minimal` (carries the guest agent).
  Managed base ARN per region: `arn:aws:lambda:<region>:aws:microvm-image:al2023-1`.
- **No SDL2 in AL2023 repos (no EPEL).** The Dockerfile compiles the whole stack from
  source: **SDL2 2.30.9** (X11 video + PulseAudio/ALSA audio; GL/Wayland/Vulkan
  disabled to avoid mesa-devel), **SDL2_mixer 2.8.0** (music codecs off; WAV SFX is what
  DOOM needs), **SDL2_net 2.2.0**, then **Chocolate Doom 3.0.1**. Chocolate Doom needs
  `./configure CFLAGS="-O2 -fcommon"` — modern gcc defaults to `-fno-common`, which
  rejects the legacy tentative definition of `demoextend` (only `chocolate-hexen`
  actually fails, but it aborts the build).
- **Display:** `Xvnc :1` at **640x400** to match Chocolate Doom's window, so the whole
  desktop IS DOOM and the render gate can detect it. A larger root leaves DOOM in a
  corner AND starves the brightness probe (it samples mostly the black border).
- **Stream servers** (`rootfs/opt/capsule/`):
  - `video_ws.py` `:6903` — ffmpeg `x11grab` → libx264 (`ultrafast`, `zerolatency`,
    baseline) → Annex-B access units over a binary WebSocket; browser decodes with
    WebCodecs `VideoDecoder` to a `<canvas>`.
  - `audio_ws.py` `:6902` — `parec` on `capsule.monitor` → Opus 96 kbps / 20 ms →
    Ogg demuxed to raw Opus packets; browser decodes with WebCodecs `AudioDecoder`.
  - `input_ws.py` `:6904` — JSON key/mouse events → `python-xlib` XTEST `fake_input`.
  - `websockify`/noVNC `:6901` — single-port display fallback (used for headless pixel
    checks via `/vnc.html`).
- `run_app.sh` launches `chocolate-doom -iwad $WAD -fullscreen -nograbmouse` with
  `SDL_VIDEODRIVER=x11` and `SDL_AUDIODRIVER=pulse` (audio routes to the `capsule`
  null-sink that `audio_ws` captures).
- **`focus.py` is REQUIRED for input.** There is no window manager, so nothing assigns X
  keyboard focus and XTEST keystrokes land nowhere. `focus.py` polls every few seconds
  and `XSetInputFocus` to the window whose name contains "doom".
- `start.sh` runs the **:9000 hook responder**, held at 503 until the display is up AND
  DOOM has actually **drawn** a non-black frame (python-xlib center-crop mean ≥ threshold),
  so the snapshot freezes a live game, not a blank screen.
- `capsule/app/` holds the staged `DOOM1.WAD` (gitignored, never committed). A leftover
  `DOOM.EXE` sits there too but is **unused** by the native path.

## The Fork B proxy (how the browser reaches the MicroVM — `rs-cli/src/proxy.rs`)

The MicroVM endpoint requires a JWE auth token in the **`X-aws-proxy-auth` header** on
every request, including WebSocket upgrades and plain navigations (the query-string form
returns 403). Browsers cannot set that header, so a browser cannot authenticate to the
endpoint on its own. `ldoom open` runs a tiny reverse proxy on **`127.0.0.1:6080`**: the
browser speaks plain HTTP/WS to loopback, and the proxy injects the auth headers and
forwards over TLS to `https://<id>.lambda-microvm.<region>.on.aws`. **The JWE lives only
in the proxy; the browser never sees it.**

It is a `hyper` 1.x server. Each request takes one of three paths:

1. **`/__lambdadoom/*` → local control plane.** Never forwarded upstream. Drives
   suspend/resume/state via SigV4 (works while the VM is frozen). This is what the
   injected control bar calls. **Hardened against CSRF / DNS-rebinding** (it runs AWS
   calls with the user's creds): the `Host` header must be loopback, the `Origin` (if
   present) must be our loopback origin, the request must carry the per-session HttpOnly
   `ldoom_control` cookie, and `suspend`/`resume` require `POST`. A cross-origin,
   rebound, or blind local request is rejected with 403. (Residual: a same-user process
   that can scrape the browser/proxy session is still out of scope. See `is_loopback_authority` and
   `cookie_has_control_secret` in `rs-cli/src/proxy.rs`.)
2. **WebSocket upgrade → `handle_ws`.** Dials the upstream `wss://<endpoint><path>` with
   `tokio-tungstenite`, carrying `X-aws-proxy-auth` + `X-aws-proxy-port` + any requested
   subprotocol; answers the browser handshake locally (derives `Sec-WebSocket-Accept`),
   takes over the browser TCP via `hyper::upgrade::on`, and **pumps frames both ways**
   until either side closes. Each live pump increments the activity counter (idle-suspend).
3. **Plain HTTP → `handle_http`.** Rebuilds the request against the upstream, drops
   hop-by-hop + `Host` headers, adds the two auth headers, forwards via `reqwest`, streams
   the response back. On the root HTML it splices in the Suspend/Resume control bar.

**Port routing.** One loopback origin fans out to the four internal services via a
path-prefix table that sets `X-aws-proxy-port`: `/ldoom/audio`→6902,
`/ldoom/video`→6903, `/ldoom/input`→6904, everything else → the display port **6901**.

**The noVNC display path (single-socket, simplest):**
1. Browser loads `http://127.0.0.1:6080/vnc.html` (+ assets) over HTTP. The proxy forwards
   each to the endpoint with `X-aws-proxy-port: 6901`, where **websockify** serves the
   noVNC client files.
2. noVNC opens **one WebSocket** (`/websockify`). The proxy upgrades it and dials `wss://`
   to the endpoint (still port 6901); inside the VM websockify bridges that WS to **Xvnc
   :1** (the VNC server).
3. That single RFB WebSocket carries **both directions**: framebuffer updates (DOOM's
   pixels) VM→browser, and keyboard/pointer events browser→VM (noVNC has input built in).
   So the noVNC path needs no separate input/audio channel — it is self-contained on 6901.
   This is what the headless pixel checks use (`/vnc.html`).

The `?display=h264` path is different: it uses *three* WebSockets — video 6903, audio
6902, input 6904 — for much lower egress, with WebCodecs decoding in the browser. noVNC on
6901 is the simpler one-socket fallback.

**TLS boundary.** Browser ↔ proxy is plain HTTP/WS on loopback (never leaves your machine,
so no TLS needed). Proxy ↔ endpoint is HTTPS/WSS; the endpoint terminates external TLS.

**Survives resume.** The upstream host + token live behind `RwLock`s the forward path
reads per request. On resume the control handler re-mints the token and swaps in the
(possibly moved) endpoint, so the reloaded page reconnects with no proxy restart.

## Verified ground truth (the real API — design around these)

These were verified live; trust them.

**Endpoint & signing**
- SigV4 signing name = `lambda`. Control host = `https://lambda.{region}.amazonaws.com`.
  API path prefix `/2025-09-09/`. Streaming endpoint: `<id>.lambda-microvm.{region}.on.aws`.

**Operations**
- `CreateMicrovmImage` — POST `/2025-09-09/microvm-images`; poll `GetMicrovmImage` to
  **CREATED** / CREATE_FAILED.
- `RunMicrovm` — POST `/2025-09-09/microvms`; PENDING → RUNNING → SUSPENDING →
  SUSPENDED → TERMINATING → TERMINATED.
- `GetMicrovm`, `ListMicrovms` (exists), `ListMicrovmImages` (GET
  `/2025-09-09/microvm-images`, returns only live images), `ListManagedMicrovmImages`.
- `CreateMicrovmAuthToken` — POST `.../microvms/{id}/auth-token`; body
  `allowedPorts:[{port}]` + `expirationInMinutes` ≤ 60; returns an `authToken` map, use
  the value at key **`X-aws-proxy-auth`**.
- `SuspendMicrovm` / `ResumeMicrovm` — POST `.../{id}/suspend|resume`.
- **`TerminateMicrovm` = DELETE `.../microvms/{id}`.**
- **`DeleteMicrovmImage` = DELETE `.../microvm-images/{FULL-ARN}`.** The path segment
  must be the **full image ARN**, not the bare name (a name → 400 "Invalid ARN format").
  Colons in the path are fine unencoded.
- **A data-plane request to the ingress endpoint AUTO-RESUMES a SUSPENDED MicroVM**
  ("wake on traffic"). To keep a suspended capsule cheap, poll status via the **control
  plane** (`GetMicrovm` — does NOT resume), and don't let the page auto-reconnect.

**Hooks (the #1 build-failure source)**
- Lifecycle hooks are **ENABLED/DISABLED enums, NOT path strings**: `microvmImageHooks.ready`,
  `microvmHooks.run`/`resume`/…, plus `*TimeoutInSeconds` and `hooks.port`.
- **THE HOOK LISTENER MUST BE ON PORT 9000.** AWS POSTs the readiness probe to
  `http://127.0.0.1:9000/aws/lambda-microvms/runtime/v1/ready`. Wrong port → CREATE_FAILED
  "Ready hook invocation timed out".
- The snapshot is captured when `/ready` returns 200 → **hold 503 until DOOM is drawn**,
  or runs/resumes restore a blank screen. `build.rs` sets `readyTimeoutInSeconds = 600`.

**Image & base / build**
- `CreateMicrovmImage` does **not** request `additionalOsCapabilities:["ALL"]`; the capsule
  is native ARM Chocolate Doom and does not need the old x86 translation capability.
  `readyTimeoutInSeconds` is 600.
- The server-side Docker build fails **fast** (~2 min) for a compile error and **slow**
  (~6 min, runs the gate) for a render/readiness failure. Read build logs in CloudWatch
  log group `/aws/lambda-microvms/<image-name>` (the messages contain a unicode arrow, so
  on Windows set `PYTHONUTF8=1` and write to a file rather than the console).

**Networking**
- **OMIT** ingress/egress network connectors → Lambda-managed defaults (JWE-auth ingress
  + INTERNET_EGRESS). `[]` / `[""]` **fail** validation. In config they are empty strings.

## Hard constraints (design around these)

- **ARM64 only**, ≤16 vCPU / 32 GB RAM / 32 GB disk.
- **Account memory quota = 8 GB total** (each VM ≥ 2 GB; suspended VMs still hold memory).
- **Ingress is HTTPS/WSS only.** Auth = JWE in the **`X-aws-proxy-auth` header**
  (query-string JWE → 403). Port via `X-aws-proxy-port` header.
- **Native ARM only.** Keep the capsule native aarch64 with no translation layer; that
  is what keeps rendering reliable across the fleet. macOS is out of scope.
- **Snapshots** must catch DOOM **drawn** (the `/ready` 503-gate). Suspend/resume is a
  **live memory** snapshot — that is the "wow".

## Non-negotiable correctness items (don't simplify these away)

- **Render-gated snapshot.** Hold `/ready` at 503 until DOOM has actually drawn, so the
  build snapshot (and every resume) catches a live frame, not black.
- **Input focus asserter (`focus.py`).** Without it there is no window manager to give
  SDL keyboard focus, so XTEST input is silently dropped.
- **Fork B proxy is THE auth path.** Browsers can't set `X-aws-proxy-auth` and the
  query-string JWE returns 403, so `ldoom open` runs the local loopback proxy that
  injects the header. Do not relitigate Fork A.
- **CSPRNG reseed on resume — ABSENT BY DESIGN (verified 2026-06-26).** A resumed VM
  replays frozen entropy → identical randomness. The current native capsule does **no**
  reseed/TLS-bounce on resume (the `resume` hook is enabled in `build.rs` but unused for
  this). This is safe **only because AWS terminates TLS at the endpoint** — the in-VM hop
  is plain HTTP/WS, so no entropy-sensitive crypto runs in the guest. **Required before
  any future capsule terminates TLS in-VM or generates keys/nonces:** add a `resume`-hook
  step that reseeds the entropy pool and bounces the affected listener.

## Multi-region recipe (proven)

IAM roles are global, so they are reused. Per region you need only: a regional S3
artifact bucket, the region's base image ARN, and a config pointing at them.

- Buckets: `lambdadoom-artifacts-ue2-<acct>` / `...-uw2-<acct>` (us-east-1 uses the
  CloudFormation `lambdadoom-artifactbucket-...`). The build role's inline S3 policy
  (`lambdadoom-build`) was broadened to GetObject on all three bucket ARNs.
- Per-region `~/.lambdadoom/config.toml` differs ONLY in `region`, `artifact_bucket`, and
  `base_image_arn`. Connectors empty.
- **Windows gotcha:** the `directories` crate ignores `$USERPROFILE`/`$HOME`, so
  `~/.lambdadoom/config.toml` is a single fixed file. To run regions in parallel, write the
  config, launch the build, wait for its "image creating" log line (config consumed into
  memory), then overwrite for the next region. `state.json` is shared and parallel builds
  RACE its writes — rely on the build-log image ARN, not local state.

## Key decisions (locked — don't relitigate without asking)

- **CLI in Rust.** Use the AWS SDK, never shell out to the `aws` CLI for control-plane
  ops. (The `aws` CLI doesn't even have the MicroVMs model; for raw API calls use a
  signed request, e.g. the botocore SigV4 helper in scratch.)
- Depends on the official **`aws-sdk-lambdamicrovms`** crate.
- **Native ARM DOOM (Chocolate Doom).** Single app, not a desktop.
- **Display = TigerVNC (Xvnc) + the DIY H.264/Opus/XTEST servers** + websockify/noVNC as
  the single-port fallback.
- Only ship **shareware `DOOM1.WAD`** + **GPLv2 Chocolate Doom**, fetched at build time,
  never committed. Never the retail `DOOM.WAD`/`DOOM2.WAD`.

## Conventions

- **Be lazy (ponytail).** Stdlib/native/existing components before custom code; shortest
  diff that works. Never cut the correctness items above.
- Single binary crate; no Cargo workspace. Mark deliberate shortcuts with `// ponytail:`.
- Leave one runnable check behind non-trivial logic. Capsule scripts MUST be LF
  (`.gitattributes` enforces it) or the guest fails on a `\r` shebang.

## Credentials / running against the cloud (Windows)

- Granted `assume --export <profile>` writes static creds to `~/.aws/credentials`
  (profile `test_AccountA/AdministratorAccess`; SSO tokens expire ~hourly, re-auth when
  they lapse). `scratchpad/awsenv.ps1` dot-sources those into env for a PowerShell session.
- The Rust SDK on Windows can't use the SSO login-session profile directly; export creds
  to env (or use `awsenv.ps1`) and clear `AWS_PROFILE` for cloud runs.

## Commands

```
ldoom build --name N        # zip capsule/ → S3 → CreateMicrovmImage → poll CREATED
ldoom up    --name N        # RunMicrovm → poll RUNNING
ldoom open  --name N        # mint token → Fork B loopback proxy → open 127.0.0.1:6080
                           #   (--no-open serves the proxy without launching a browser)
ldoom suspend|resume --name N
ldoom down  --name N        # terminate (DELETE)
ldoom ps [--refresh]        # list (state.json + ListMicrovms)
```

State: `~/.lambdadoom/config.toml` (region, bucket, role ARNs, ports, `display`) and
`~/.lambdadoom/state.json` (name → {image_arn, microvm_id, endpoint, state}).

Open the h264 display (audio + input) at `http://127.0.0.1:6080/?display=h264`; the
single-port noVNC pixel-check is `http://127.0.0.1:6080/vnc.html`.

### In-browser Suspend/Resume (no extra command)

`ldoom open` injects a **Suspend/Resume control panel** into the served page (in the
Fork B proxy, so the capsule image is never rebuilt and any capsule gets it). The proxy
exposes local control endpoints — `GET /__lambdadoom/state`, `POST /__lambdadoom/{suspend,resume}`
— that drive the control plane with the caller's creds; the browser never sees them. See
`rs-cli/src/proxy.rs`:
- **`open` tolerates a SUSPENDED capsule** so the control bar is reachable while frozen;
  **resume re-mints the token and refreshes the (possibly moved) endpoint** so the page
  reconnects.
- **A root navigation is gated on control-plane state**: if SUSPENDED, the proxy serves a
  local Resume page instead of forwarding — otherwise the forwarded request would
  auto-resume the VM (wake-on-traffic) and silently restart billing.
