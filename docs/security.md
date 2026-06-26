# Security

LambdaDoom is single user and deploys into your own account. You run the `ldoom` CLI, it
provisions resources in your AWS account, and only you connect to the stream. It is not a
multi-tenant service and is not hardened as one. This is the honest threat model.

## Trust boundaries

```mermaid
flowchart LR
    subgraph LOCAL["Your machine"]
        BROWSER["browser tab"]
        PROXY["ldoom proxy<br/>127.0.0.1:6080"]
        CLI["ldoom CLI<br/>AWS credentials"]
        BROWSER <-->|HTTP/WS on loopback| PROXY
    end

    subgraph AWS["AWS"]
        CONTROL["Lambda MicroVMs<br/>control plane"]
        ENDPOINT["MicroVM endpoint<br/>id.lambda-microvm.region.on.aws"]
        MICROVM["Lambda MicroVM<br/>Chocolate Doom on ARM64 Firecracker"]
    end

    CLI -->|SigV4 lifecycle calls| CONTROL
    PROXY -->|HTTPS/WSS + JWE header| ENDPOINT
    ENDPOINT --> MICROVM
```

## What protects you

- **Encrypted transport.** Browser to proxy is loopback only and never leaves your machine.
  Proxy to the MicroVM endpoint is HTTPS and WSS, terminated by AWS.
- **Authenticated data plane.** The endpoint requires a JWE token in the `X-aws-proxy-auth`
  header, or it returns 403. Reaching your MicroVM requires a token minted with your AWS
  credentials, so a random person on the internet cannot.
- **Short-lived, scoped token.** `CreateMicrovmAuthToken` caps at 60 minutes and is restricted
  to ports 6901 to 6904. It is not a standing credential.
- **The browser never sees the token.** The proxy injects it server side, so it cannot leak
  into page JavaScript, history, or logs.
- **Loopback-only proxy.** It binds `127.0.0.1`, not your LAN or the internet.
- **Hardened control endpoints.** `/__lambdadoom/{state,suspend,resume}` drive the control
  plane with your credentials, so they require a loopback `Host`, a loopback `Origin` when
  present, an HttpOnly per-session `ldoom_control` cookie, and `POST` for `suspend` and
  `resume`. A cross-origin, DNS-rebound, or blind local request gets a 403. See
  `is_loopback_authority` and `cookie_has_control_secret` in `rs-cli/src/proxy.rs` and their
  tests.
- **Firecracker isolation in your own account.** No shared multi-tenant surface.
- **Least-privilege IAM.** The build role has only `s3:GetObject` on the artifact bucket plus
  CloudWatch Logs writes. The execution role has no permissions, since the MicroVM never calls
  back into AWS. The bucket is private, encrypted (AES256), and expires build contexts after
  three days.

## Residual risks and non-goals

The parts I deliberately left out of scope:

- **A local process running as you** can reach `127.0.0.1:6080` directly, since it is not
  bound by same-origin policy. The per-session `ldoom_control` cookie blocks blind calls to
  the control endpoints, but a same-user process that can scrape the browser session or proxy
  traffic is still out of scope: a process running as you already owns your shell and
  credentials. Those endpoints only suspend, resume, or read state for the one MicroVM you own,
  so the worst case is freezing or thawing your own game.
- **Your AWS credentials live on your machine** (via Granted, SSO, or environment variables),
  as with any AWS CLI or SDK use. They are never committed, and `.gitignore` excludes `.env`,
  `*.pem`, `*.key`, `aws-credentials*`, and `~/.lambdadoom/`. The binary reads credentials
  through the standard AWS chain and never writes them; `~/.lambdadoom/` holds only non-secret
  config and capsule state.
- **The prebuilt binary is a supply-chain dependency.** `deploy.sh` downloads `ldoom` from this
  repo's [GitHub Releases](https://github.com/somoore/LambdaDoom/releases), verifies the
  release SHA256 sidecar, and verifies the GitHub artifact attestation when `gh` is available.
  Release builds are produced by the [release workflow](../.github/workflows/release.yml) from
  this source. To avoid trusting a prebuilt artifact, build it yourself (`cd rs-cli && make
  release`) and point `deploy.sh` at it with `LDOOM_BIN`. The binary runs locally with your
  credentials, so only run a release you trust.
- **Entropy after a snapshot.** A resumed MicroVM replays frozen entropy, so a CSPRNG seeded
  before the snapshot repeats its output. AWS terminates TLS, so the hop inside the MicroVM is
  plain and this is not exercised here. A capsule that terminates TLS inside the MicroVM must
  reseed on resume. See section 7 of [architecture.md](architecture.md).
- **Default egress is the public internet.** Omitting the network connectors gives the
  Lambda-managed defaults (JWE-authenticated ingress, internet egress). The MicroVM can reach
  the internet; it does not need to, but it is not network-isolated by default.
- **Not multi-tenant, not production.** No auth between browser and proxy beyond loopback
  binding, no rate limiting, no audit logging. Do not expose the proxy port off your machine.

## Reporting

This is a personal demo. If you find a security issue, please open an issue or contact the repo
owner directly rather than posting a public exploit.

## Legal and scope

LambdaDoom does not include or distribute retail DOOM game assets. By default, the build process
downloads the shareware `DOOM1.WAD` and builds Chocolate Doom at image build time. See
[LEGAL.md](../LEGAL.md) for the full legal notice.
