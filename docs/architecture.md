# Architecture

How I run native DOOM inside an AWS Lambda MicroVM and stream it to a browser tab. Most of
the design came from working around the platform's constraints, which I call out as I go.

> **Status: proven end to end in three regions** (us-east-1, us-east-2, us-west-2). The
> `ldoom` CLI drives the real control plane through the full lifecycle
> (`build`, `up`, `open`, `suspend`, `resume`, `down`). Native Chocolate Doom streams with
> video, audio, and keyboard input, and suspends and resumes on a live memory snapshot. The
> API values below are ones I verified live; see
> [microvm-ground-truth.md](microvm-ground-truth.md).

**How to read this:** sections 1-3 explain the flow, sections 4-7 cover the hard parts that
made the demo work, and sections 8-9 list the implementation choices and repo layout.

---

## 1. System overview

Three pieces: a thin client (your machine), the AWS control plane (the Lambda MicroVMs
lifecycle API), and the capsule (the running MicroVM). Your machine never runs DOOM. It
drives the lifecycle and renders the stream.

```mermaid
flowchart LR
    subgraph CLIENT["Your machine (thin client)"]
        direction TB
        CLI["ldoom CLI (Rust)"]
        BROWSER["Browser tab<br/>WebCodecs + Web Audio + input"]
        PROXY["ldoom open<br/>127.0.0.1:6080 proxy<br/>injects X-aws-proxy-auth"]
    end
    subgraph AWS["AWS"]
        direction TB
        CP["Lambda MicroVMs control plane<br/>create, run, token, suspend, resume"]
        EP["Per-MicroVM HTTPS/WSS endpoint<br/>id.lambda-microvm.region.on.aws"]
        S3["S3 build artifacts"]
        subgraph CAPSULE["Capsule MicroVM (ARM64 Firecracker, AL2023)"]
            direction TB
            DOOM["Chocolate Doom<br/>native aarch64 SDL2"]
            XVNC["Xvnc :1<br/>headless X, 640x400"]
            VID["video_ws :6903<br/>x11grab to H.264"]
            AUD["audio_ws :6902<br/>PulseAudio to Opus"]
            INP["input_ws :6904<br/>JSON to XTEST"]
            HOOK["hook responder :9000<br/>/ready render gate"]
        end
    end
    CLI -->|"AWS SDK / SigV4"| CP
    CLI -->|"zip context"| S3
    CP -->|"manages, /ready hook :9000"| CAPSULE
    BROWSER -->|"http/ws on loopback"| PROXY
    PROXY -->|"TLS + JWE header"| EP
    EP --> VID
    EP --> AUD
    EP --> INP
    DOOM --> XVNC
    XVNC --> VID
    DOOM --> AUD
    INP --> XVNC
    classDef client fill:#1f2937,stroke:#60a5fa,color:#e5e7eb
    classDef aws fill:#0f3a2e,stroke:#34d399,color:#d1fae5
    classDef vm fill:#3a2a0f,stroke:#fbbf24,color:#fef3c7
    class CLI,BROWSER,PROXY client
    class CP,EP,S3 aws
    class DOOM,XVNC,VID,AUD,INP,HOOK vm
```

---

## 2. The platform constraints

Lambda MicroVMs are opinionated. Each constraint below shaped the design.

| Constraint | What it meant for the design |
|---|---|
| **ARM64 only** | DOOM is a native aarch64 build (Chocolate Doom), so it runs directly on the ARM CPU with no translation layer and renders reliably across the fleet. macOS is out of scope. |
| **No SDL2 in Amazon Linux 2023** (no EPEL) | The Dockerfile compiles the SDL stack from source: SDL2, SDL2_mixer, SDL2_net, then Chocolate Doom. |
| **HTTPS and WSS ingress only** (no raw TCP) | Everything is tunnelled over WebSockets: H.264 video, Opus audio, and a JSON input channel, plus a noVNC fallback. |
| **Auth is the `X-aws-proxy-auth` header** | The token has to be in the header; the same token in a query string returns 403. Browsers cannot set the header, so I run a loopback proxy (section 5). |
| **Lifecycle hooks are ENABLED/DISABLED toggles** | Hooks are switches, not script paths: `microvmImageHooks.ready`, `microvmHooks.run` and `resume`, plus `*TimeoutInSeconds` and `hooks.port`. I send `readyTimeoutInSeconds=600`. |
| **The ready hook has to be on port 9000** | AWS POSTs its readiness probe to `http://127.0.0.1:9000/aws/lambda-microvms/runtime/v1/ready`. Any other port fails the build with "Ready hook invocation timed out". |
| **Snapshot based** | DOOM has to be drawn at snapshot time (the ready render gate, section 4), or every run and resume restores a blank screen. Suspend and resume is a live memory snapshot. |
| **8 GB account memory quota** | Each MicroVM is at least 2 GB, and suspended MicroVMs still hold their memory, so plan for a few at a time, not dozens. |

---

## 3. Lifecycle

### 3.1 Build

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant CLI as ldoom CLI
    participant S3 as S3
    participant CP as MicroVMs control plane

    U->>CLI: ldoom build --name demo
    CLI->>CLI: zip capsule (Dockerfile + rootfs + DOOM1.WAD)
    CLI->>S3: put_object context.zip
    CLI->>CP: create_microvm_image (base image, build role, readyTimeout 600)
    Note over CP: builds the Dockerfile (compiles SDL2 + Chocolate Doom),<br/>boots, snapshots when /ready on :9000 returns 200
    loop poll until terminal
        CLI->>CP: get_microvm_image
        CP-->>CLI: CREATING then CREATED (or CREATE_FAILED)
    end
    CLI->>CLI: persist image_arn to state.json
```

> The snapshot is captured the moment `/ready` returns 200, so the capsule holds `/ready` at
> 503 until DOOM has drawn a non-black frame (a python-xlib center-crop brightness probe).
> That way the snapshot always catches a live game. The build runs two concurrent boots and
> both have to reach ready.

### 3.2 Run and open

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant CLI as ldoom CLI
    participant CP as MicroVMs control plane
    participant BR as Browser
    participant EP as MicroVM endpoint

    U->>CLI: ldoom up --name demo
    CLI->>CP: run_microvm (image, idle policy)
    loop poll until RUNNING
        CLI->>CP: get_microvm
    end

    U->>CLI: ldoom open --name demo
    CLI->>CP: create_microvm_auth_token (30 min, ports 6901-6904)
    CP-->>CLI: JWE token (X-aws-proxy-auth)
    CLI->>CLI: start 127.0.0.1:6080 reverse-proxy (Fork B)
    CLI->>BR: open http://127.0.0.1:6080/?display=h264
    BR->>CLI: http/ws to loopback proxy
    CLI->>EP: TLS + JWE header (plus X-aws-proxy-port per stream)
    EP-->>BR: H.264 video + Opus audio, input flows back
    Note over U,BR: DOOM appears in the browser, with sound and controls
```

### 3.3 Suspend and resume

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant CLI as ldoom CLI / control bar
    participant CP as MicroVMs control plane
    participant CAP as Capsule

    U->>CLI: Suspend
    CLI->>CP: suspend_microvm
    Note over CAP: live memory + disk frozen (compute billing stops)
    CP-->>CLI: SUSPENDED

    U->>CLI: Resume
    CLI->>CP: resume_microvm
    Note over CAP: restore frozen memory, re-mint token, refresh endpoint
    CP-->>CLI: RUNNING
    Note over U: DOOM exactly where you left it (same frame, health, ammo)
```

Suspend and resume are control-plane calls, so the in-page control panel (injected by the
proxy, section 5) keeps working even while the MicroVM is frozen and the stream is dead.

---

## 4. Inside the capsule

`start.sh` brings the stack up in dependency order, runs the readiness responder on `:9000`,
and holds `/ready` at 503 until DOOM is drawn. The display is Xvnc `:1` at 640x400 to match
Chocolate Doom's window, so the whole desktop is DOOM and the render probe reads a real frame
instead of a black border.

```mermaid
flowchart TB
    START["start.sh supervisor"]
    START -->|"1"| HK["hook responder :9000<br/>/ready 503 to 200"]
    START -->|"2"| XV["Xvnc :1<br/>headless X, 640x400"]
    START -->|"3"| SRV["video_ws :6903, audio_ws :6902<br/>input_ws :6904, noVNC :6901"]
    START -->|"4"| DOOM["chocolate-doom -fullscreen"]
    START -->|"5"| FOCUS["focus.py asserts X input focus"]
    DOOM -->|"renders"| XV
    DOOM -->|"sound"| SRV
    XV -->|"grabbed and served by"| SRV
    FOCUS -->|"XSetInputFocus"| XV
    SRV -->|"the only exposed ports"| OUT["MicroVM endpoint :6901-6904"]
    DOOM --> GATE["DOOM drawn? probe above threshold"]
    GATE -->|"yes, flip /ready to 200"| HK
    HK -->|"AWS POSTs /ready, gets 200"| READY["capsule READY, safe to snapshot"]
    classDef p fill:#3a2a0f,stroke:#fbbf24,color:#fef3c7
    class START,HK,XV,SRV,DOOM,FOCUS,OUT,GATE,READY p
```

Four small servers replace an off-the-shelf remote desktop, since none fit all the
constraints at once (headless, no systemd, ARM64, a license I could ship):

- **`video_ws.py` on :6903** grabs the screen with ffmpeg `x11grab` and encodes it with
  libx264 (`ultrafast`, `zerolatency`, baseline). Each Annex-B access unit is one binary
  WebSocket frame, decoded in the browser by the WebCodecs `VideoDecoder` onto a `<canvas>`.
- **`audio_ws.py` on :6902** runs `parec` on the PulseAudio sink monitor, encodes Opus at
  96 kbps in 20 ms frames, and sends raw Opus packets, decoded by the WebCodecs `AudioDecoder`.
- **`input_ws.py` on :6904** turns JSON key and mouse events into XTEST `fake_input` calls
  through python-xlib.
- **noVNC and websockify on :6901** is a single-socket fallback that carries display and
  input over one connection. The headless pixel checks use it.

`focus.py` is load-bearing. There is no window manager, so nothing assigns X keyboard focus
and XTEST keystrokes would land nowhere. It polls and calls `XSetInputFocus` on the game
window so input reaches DOOM.

---

## 5. The proxy

The MicroVM endpoint authenticates with a token (a JWE) in the `X-aws-proxy-auth` header and
selects the target port with `X-aws-proxy-port`. The same token in a query string returns
403, and browsers cannot set headers on a navigation or a `WebSocket`, so a tab cannot
authenticate to the endpoint on its own. So `ldoom open` runs a loopback reverse-proxy on
`127.0.0.1:6080`: the browser talks plain HTTP and WebSocket to loopback, and the proxy
injects the headers and forwards over TLS. The token lives only in the proxy.

The proxy (`rs-cli/src/proxy.rs`, on `hyper` 1.x) handles a request one of three ways:

1. **`/__lambdadoom/*` goes to the local control plane.** Never forwarded. Drives suspend,
   resume, and state with SigV4 (which works while the MicroVM is frozen) and powers the injected
   control panel.
2. **A WebSocket upgrade** dials the upstream `wss://` with the auth and port headers, answers
   the browser handshake locally, and pumps frames both ways until either side closes.
3. **Plain HTTP** is rebuilt against the upstream with the auth headers, and on the served
   page the proxy splices in the control panel.

A path-prefix table maps one loopback origin onto the four internal services via
`X-aws-proxy-port`: `/ldoom/audio` to 6902, `/ldoom/video` to 6903, `/ldoom/input` to 6904,
and everything else to the display port 6901.

The `/__lambdadoom/*` endpoints run AWS calls with your credentials, so they are hardened
against CSRF, DNS rebinding, and blind local calls: the `Host` header must be loopback, the
`Origin` (when present) must be the loopback origin, the request must carry the per-session
HttpOnly `ldoom_control` cookie, and `suspend` and `resume` require `POST`. Full threat model
in [security.md](security.md).

---

## 6. The bandwidth budget

The endpoint caps traffic at a size-dependent rate, so the codecs are a design lever, not a
finishing touch. H.264 with motion-compensated P-frames (rather than VNC dirty rectangles)
cuts the video bytes a lot for full-motion content, and Opus at 96 kbps (rather than raw PCM)
cuts the audio bytes by about 13 to 15 times. Suspend-on-idle plus wake-on-traffic means an
unwatched tab is not burning a running MicroVM; the proxy tracks live WebSocket sessions to
drive that idle suspend.

---

## 7. The snapshot hazard

Two snapshot-specific concerns:

- **Render at snapshot.** The `/ready` 503 gate holds until DOOM is drawn, so neither a cold
  run nor a resume restores a blank screen. Enforced, not hoped for.
- **Entropy on resume.** A resumed MicroVM replays frozen entropy, so a CSPRNG seeded before the
  snapshot would produce the same bytes twice, a real problem for TLS terminated inside the
  MicroVM. In LambdaDoom the endpoint terminates TLS, so the hop inside the MicroVM is plain and
  this is not exercised. A future capsule that terminates TLS inside the MicroVM should reseed
  the entropy pool and bounce the listener on resume.

Full threat model in [security.md](security.md).

---

## 8. Why I chose each piece

| Concern | Choice | Why |
|---|---|---|
| DOOM on ARM64 | **Chocolate Doom** (native aarch64, SDL2) | Runs natively on ARM with no translation layer, and SDL2 gives real audio and input. Compiled from source because AL2023 ships no SDL2. |
| Display | **Xvnc (TigerVNC) at 640x400** | A headless X server sized to the engine window, so the whole desktop is DOOM and the render gate can detect it. |
| Video | **ffmpeg x11grab to libx264 to WebCodecs** | An inter-frame codec over WebSockets, far less egress than VNC dirty rectangles for full-motion content. |
| Audio | **PulseAudio to Opus to WebCodecs** | About 13 to 15 times less egress than raw PCM at transparent quality. |
| Input | **XTEST (python-xlib) over WebSockets** | A reverse channel for the H.264 path, with `focus.py` keeping the game window focused. |
| Stream auth | **Fork B loopback proxy** (`ldoom open`) | Browsers cannot set `X-aws-proxy-auth`, so a header-injecting loopback proxy is the only path that works. |
| CLI | **Rust** | One distributable binary using the official `aws-sdk-lambdamicrovms` crate (no `aws` CLI shell-out). SigV4 signing name is `lambda`. Ships prebuilt via GitHub Releases for Linux, macOS, and Windows on x86_64 and arm64. Source in `rs-cli/`. |
| Infra | **CloudFormation** (`deploy/doom.yaml`) | One self-serve stack: an S3 artifact bucket and the build and execution IAM roles. |

---

## 9. Repository layout

```
LambdaDoom/
├── deploy.sh               # one-command front door: deploy, fetch binary, build/up/open
├── uninstall.sh            # remove everything (MicroVM, image, stack, binary, local state)
├── deploy/doom.yaml        # CloudFormation: S3 bucket + IAM build/exec roles (Launch Stack)
├── capsule/                # the MicroVM image
│   ├── Dockerfile          #   compiles pinned SDL2 + Chocolate Doom, verifies WAD SHA256
│   ├── rootfs/opt/capsule/ #   start.sh, run_app.sh, focus.py, *_ws.py
│   └── app/                # optional WAD override drop zone (gitignored)
├── rs-cli/                 # the Rust `ldoom` CLI (shipped prebuilt; build only if you want)
│   ├── Cargo.toml  Makefile
│   └── src/                #   main, config, state, aws, poll, browser, proxy, commands/
└── docs/                   # architecture, security, ground truth, media, generalizing
```

LambdaDoom depends on the official `aws-sdk-lambdamicrovms` crate from crates.io. The capsule
pins and SHA256-verifies every external tarball/WAD it downloads during the image build.
