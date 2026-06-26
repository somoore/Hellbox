# Lambda MicroVMs: verified ground truth

The facts below were verified against the live AWS Lambda MicroVMs service while
building LambdaDoom. They are the load-bearing details: get one wrong and a build
fails or a request 403s. This is a working reference, not the official docs; check
the AWS docs for anything not listed here.

## Endpoint and signing

- **Control plane:** SigV4-signed REST. Signing name `lambda`, host
  `https://lambda.<region>.amazonaws.com`, API path prefix `/2025-09-09/`.
- **Data plane (the stream):** a per-VM hostname, `<id>.lambda-microvm.<region>.on.aws`.
- **Ingress is HTTPS / WSS only** (HTTP/1.1, HTTP/2, WebSockets, gRPC, SSE). No raw
  inbound TCP or UDP.

## Authentication

- A data-plane request needs a JWE auth token in the **`X-aws-proxy-auth` header**.
  The same token in a query string returns **403**. Browsers cannot set that header,
  which is why `ldoom open` runs a local header-injecting proxy (see architecture.md).
- Select the internal port with the **`X-aws-proxy-port` header** (or the WebSocket
  subprotocol `lambda-microvms.port.<N>`).
- Mint a token with `CreateMicrovmAuthToken` (`POST .../microvms/{id}/auth-token`):
  body `allowedPorts:[{port}]` + `expirationInMinutes` (<= 60). The response is an
  `authToken` map; use the value at key `X-aws-proxy-auth`.

## Operations

- `CreateMicrovmImage` — `POST /2025-09-09/microvm-images`; poll `GetMicrovmImage`
  until state `CREATED` or `CREATE_FAILED`.
- `RunMicrovm` — `POST /2025-09-09/microvms`; states `PENDING -> RUNNING ->
  SUSPENDING -> SUSPENDED -> TERMINATING -> TERMINATED`.
- `GetMicrovm`, `ListMicrovms`, `ListMicrovmImages` (`GET /2025-09-09/microvm-images`,
  returns only live images), `ListManagedMicrovmImages`.
- `SuspendMicrovm` / `ResumeMicrovm` — `POST .../{id}/suspend|resume`.
- `TerminateMicrovm` = **DELETE** `.../microvms/{id}`.
- `DeleteMicrovmImage` = **DELETE** `.../microvm-images/{FULL-ARN}` — the path segment
  must be the full image ARN, not the bare name (a name returns 400 "Invalid ARN
  format"); colons in the path are fine unencoded.
- **Wake on traffic:** any data-plane request to a SUSPENDED VM auto-resumes it. To
  keep a suspended VM paused, poll state via the control plane (`GetMicrovm` does not
  resume), not by hitting the endpoint.

## Lifecycle hooks (the #1 build-failure source)

- Hooks are **ENABLED / DISABLED enums, not path strings**: `microvmImageHooks.ready`,
  `microvmHooks.run` / `resume` / ..., plus `*TimeoutInSeconds` and `hooks.port`.
- **The hook listener must be on port 9000.** AWS POSTs the readiness probe to
  `http://127.0.0.1:9000/aws/lambda-microvms/runtime/v1/ready`. A wrong port fails the
  build with "Ready hook invocation timed out".
- The image snapshot is captured the instant `/ready` returns 200, so **hold 503 until
  the app has actually drawn**, or every run and resume restores a blank screen.

## Image, base, and constraints

- Dockerfile base: `public.ecr.aws/lambda/microvms:al2023-minimal` (carries the guest
  agent). Managed base ARN per region: `arn:aws:lambda:<region>:aws:microvm-image:al2023-1`.
- **ARM64 only**, up to 16 vCPU / 32 GB RAM / 32 GB disk per VM.
- **Account memory quota is 8 GB total** (each VM is >= 2 GB, and suspended VMs still
  hold their memory).
- **Amazon Linux 2023 has no SDL2 in its repos** (no EPEL), so anything SDL-based is
  compiled from source. AL2023 is glibc 2.34, roughly RHEL/Rocky 9.

## Networking

- **Omit** ingress / egress network connectors to get the Lambda-managed defaults
  (JWE-auth ingress + internet egress). Passing `[]` or `[""]` fails validation.

## ARM64 and the Graviton fleet

The MicroVM fleet runs on AWS Graviton (a mix of generations). LambdaDoom runs DOOM as a
**native aarch64** build, so it executes directly on the ARM CPU with no translation layer
and renders reliably on every host. Run native ARM code; if you need something that is not
native ARM, validate it across Graviton generations, not just across regions.
