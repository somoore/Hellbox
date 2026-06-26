# LambdaDoom — Security & Architecture Review

**Date:** 2026-06-26
**Scope:** Rust CLI (`rs-cli/`), the loopback proxy, the capsule runtime (`capsule/`),
infra (`deploy/doom.yaml`), and the deploy/release supply chain.
**Method:** Full manual read of every source file, plus `cargo deny check advisories`
and an import audit of the capsule scripts. The capsule changes (L1) were **verified live**
on 2026-06-26 via a full `build → up → open` cycle in `us-east-2`: image reached CREATED
(proving the readiness hook still works on loopback) and all three stream WebSockets streamed
data through the proxy with the services bound to `127.0.0.1` (proving the AWS ingress reaches
in-guest services via loopback).

## Threat model (read this first — it calibrates every severity below)

`docs/security.md` declares an honest model: **single-user, your-own-AWS-account,
loopback-only**. You run `ldoom`, it provisions in your account, only you connect. It is
explicitly **not** multi-tenant and not hardened as a service. A process already running as
you (which owns your shell and AWS creds) is out of scope by design.

Against that model the proxy is **genuinely well-hardened** and several "obvious" attacks do
not apply — see *Verified-secure* at the bottom. **There are no Critical or High findings,
and that is the correct result, not a gap in the review.** Everything below is Medium or
lower: real, worth fixing, but bounded by the single-user/loopback boundary. The findings are
process gaps (CI), defense-in-depth hardening, and attack-surface reduction.

Each finding lists **What / Where / Why / Fix**, sorted by severity.

> **Remediation status (2026-06-26).** All findings below have been triaged and acted on.
> `[FIXED]` = code/config change applied; `[FIXED: docs]` = the fix was documentation
> because the mechanism already existed; `[RETRACTED]` = finding was wrong on closer
> inspection; `[VERIFIED]` = capsule change confirmed working against a live build. See each
> entry. One **new** finding (M3) was discovered during live testing.

---

## MEDIUM

### M1 — CI has no automated vulnerability (CVE/RUSTSEC) gate  `[FIXED]`

- **What:** Despite `Cargo.toml` going to real lengths to dodge the vulnerable rustls 0.21 /
  webpki stack, nothing in CI would *catch a future* vulnerable dependency. `cargo-deny` is
  wired up for **licenses only**, and `deny.toml` has no `[advisories]` section.
- **Where:** `.github/workflows/ci.yml:41` (`command-arguments: licenses`); `deny.toml`
  (no `[advisories]` table); the careful TLS-stack pinning in `rs-cli/Cargo.toml:29-40,89-115`.
- **Why:** The hand-tuned dep tree (e.g. avoiding RUSTSEC-flagged `rustls-webpki`) is a
  point-in-time fix with no regression guard. A `cargo update` or a transitive bump could
  re-introduce a known-vulnerable crate and CI would stay green. The `ldoom` binary runs
  locally with your AWS credentials, so a compromised dep is a credential-theft path.
- **Status check:** I ran `cargo deny check advisories` against the committed `Cargo.lock` —
  it reported **`advisories ok`**, so this is a *process* gap (no live CVE today), not a
  concrete vuln. It should be locked in before it becomes one.
- **Fix applied:** `ci.yml` now runs `command-arguments: advisories licenses` (the
  cargo-deny action already sets `command: check`, so `advisories licenses` are the
  subcommands — *not* `check advisories licenses`, which would double the `check`). No
  `[advisories]` table is needed for cargo-deny 0.19.x defaults. Verified locally:
  `cargo deny check advisories licenses` → `advisories ok, licenses ok`. Also added a weekly
  `schedule:` trigger so a newly-disclosed CVE in an unchanged lockfile surfaces without a
  push. Job renamed `rust-licenses` → `rust-deny`.

### M2 — "Orphaned Python wheels"  `[RETRACTED — finding was wrong]`

- **Original claim:** `redis`, `jwcrypto`, `cryptography` (+ `cffi`/`pycparser`) are pinned
  in `requirements.txt` but imported by no capsule code, so they could be dropped to shrink
  the attack surface.
- **Why it was wrong:** They are **real transitive dependencies of `websockify`**, not
  orphans. Primary-source check — PyPI `requires_dist` for `websockify==0.13.0` is
  `[numpy, requests, jwcrypto, redis]`, and `jwcrypto → cryptography → cffi → pycparser`. The
  Dockerfile installs with `--require-hashes` and **no `--no-deps`**
  (`capsule/Dockerfile:32-34`), so pip resolves websockify's full dependency tree and **every
  one of those packages must have a hash present** — removing any of them breaks the image
  build. The import-grep was correct (websockify only *imports* `redis`/`jwcrypto` inside its
  token-auth plugins, which LambdaDoom doesn't use) but install-time resolution doesn't care
  about runtime imports.
- **Disposition:** **No change to `requirements.txt`.** Pruning would require
  `pip install --no-deps` plus manually curating websockify's actually-imported subset, then a
  full server-side aarch64 build + render-gate + noVNC verification to prove nothing broke —
  unverifiable locally and not worth risking a confirmed-working capsule for a Low-value
  surface trim. **Verification-gated future optimization, not shipped.**

### M3 — Stale `cryptography` wheel hash breaks the capsule build on a modern host  `[FIXED + VERIFIED]`

- **What:** `capsule/requirements.txt` pinned a single sha256 for `cryptography==49.0.0` that
  matches only the older `manylinux_2_17`/`manylinux2014` wheel. On a current build host pip
  selects the newer-glibc `manylinux_2_34` wheel (different hash), and under `--require-hashes`
  this **fails the build** with "THESE PACKAGES DO NOT MATCH THE HASHES FROM THE REQUIREMENTS
  FILE." This is a **pre-existing breakage on `main`**, unrelated to the security review — it
  was surfaced when the first live build in this session failed at the pip stage (~1m50s).
- **Where:** `capsule/requirements.txt:9-10` (single-hash `cryptography` pin);
  `capsule/Dockerfile:32-34` (`pip install --require-hashes`). Observed in the CloudWatch log
  for image `l1test`: expected `36d1709f…`, got `ccac2bfe…` (the `manylinux_2_34` wheel).
- **Why:** `--require-hashes` is strict and correct (it's the supply-chain control), but a
  single platform hash is brittle: any host glibc that shifts pip's wheel selection breaks the
  build for everyone. The whole project becomes unbuildable until the hash is refreshed.
- **Fix applied + verified:** Added the other published aarch64 `cp311-abi3` glibc hashes for
  `cryptography==49.0.0` (`manylinux_2_17`, `manylinux_2_28`, `manylinux_2_34`, musllinux) so
  pip accepts whichever wheel matches the host. Verified: the next build (`l1test2`) passed the
  pip stage and reached CREATED. (`cffi`, `charset-normalizer`, `numpy`, `websockets` have the
  same single-glibc-hash shape but only their *musllinux* alternate is unlisted, which AL2023's
  glibc host never selects — so they are not currently broken. Worth multi-hashing them
  proactively if the build host ever changes; logged here, not changed.)

---

## LOW

### L1 — In-VM stream services bind `0.0.0.0` with no per-service authentication  `[FIXED + VERIFIED]`

- **What:** `video_ws` (6903), `audio_ws` (6902), `input_ws` (6904), the readiness hook
  (9000), and `Xvnc` (`-SecurityTypes None`, 5901→6901) all listen on `0.0.0.0` inside the
  MicroVM with no auth of their own. `input_ws` in particular injects arbitrary
  keyboard/mouse via XTEST from any JSON it receives.
- **Where:** `capsule/.../input_ws.py:88`, `video_ws.py:117`, `audio_ws.py:115`
  (`websockets.serve(..., "0.0.0.0", PORT)`); `start.sh:34` (`Xvnc ... -SecurityTypes None`);
  `start.sh:26` (hook on `("0.0.0.0", 9000)`).
- **Why:** The only thing keeping the public internet off these ports is the **AWS ingress
  JWE auth + the token's `allowedPorts` scoping** (minted in `open.rs:42-44` /
  `proxy.rs:783`). That is a strong control, but it is a single layer enforced *outside* the
  VM. If a future change broadened egress/ingress, or a co-resident service on the VM were
  compromised, there is no second factor. This is a **defense-in-depth gap**, not an open
  door.
- **Fix — shipped AND live-verified (both halves):**
  1. **Hook :9000 narrowed to loopback.** AWS probes the readiness hook at
     `http://127.0.0.1:9000/...` (loopback — `docs/architecture.md:80`, `CLAUDE.md`), and
     :9000 is never in the minted token's `allowedPorts`. Done in `capsule/.../start.sh`
     (`ThreadingHTTPServer(("127.0.0.1", 9000), ...)`). **Verified:** the test image reached
     CREATED, which only happens when the render-gated `/ready` 200 is delivered to AWS via
     this hook — so loopback binding does not break readiness.
  2. **Stream services 6902/6903/6904 narrowed to loopback.** Done in `video_ws.py`,
     `audio_ws.py`, `input_ws.py` (`websockets.serve(handler, "127.0.0.1", PORT)`).
     **Verified:** with the VM RUNNING and the Fork B proxy up, all three channels streamed
     through the proxy — `/ldoom/video` returned 41,671 bytes of H.264, `/ldoom/audio`
     returned the 19-byte OpusHead config frame, `/ldoom/input` accepted injected key events.
     This proves the AWS MicroVM ingress reaches in-guest services **via loopback**, so the
     bind change is safe — it was the one open question that blocked shipping this half, now
     answered empirically.
  3. **Load-bearing invariant documented in code.** A `SECURITY:` comment at the token-mint
     site (`open.rs`) states that `allowedPorts` scoping is the control keeping the in-VM
     services off the internet, and that 9000/5901 must never be added.
- **Net effect:** every in-VM listener except websockify/Xvnc on :6901 (the display port,
  which must stay reachable for the noVNC fallback) is now loopback-only. The externally
  reachable surface is reduced to exactly the four token-scoped ports, all behind the AWS JWE
  ingress.

### L2 — Default egress is the public internet  `[FIXED: docs]`

- **What:** Network connectors are intentionally omitted, so the MicroVM gets the
  Lambda-managed default of **`INTERNET_EGRESS`**. The capsule does not need outbound
  internet at runtime (the WAD and engine are baked at build time).
- **Where:** `up.rs:48-53` (connectors only set when non-empty); `config.rs:23-25` (default
  empty); documented in `docs/security.md:71-73`.
- **Why:** A compromised in-VM process (e.g. via a malicious WAD or a stream-service bug)
  could exfiltrate or beacon outbound. Documented as a non-goal, so this is informational
  hardening.
- **Fix applied (docs):** The mechanism already existed — `config.rs` has
  `egress_connector_arn` and `up.rs:48-53` wires any non-empty value into `RunMicrovm`. So the
  real fix was telling users how to use it: `docs/security.md` now documents setting
  `egress_connector_arn` to a deny-all connector to lock egress down. No code change needed.

### L3 — No CSPRNG reseed on resume (documented, currently not exercised)  `[FIXED: docs]`

- **What:** A resumed MicroVM replays frozen entropy — a CSPRNG seeded before the snapshot
  repeats its output. There is **no reseed/listener-bounce hook in the current native
  capsule** (`run`/`resume` hooks are enabled in `build.rs:58-69` but the capsule scripts do
  no reseed on resume).
- **Where:** `build.rs:58-69` (resume hook enabled but unused for reseed); no reseed logic in
  `capsule/rootfs/opt/capsule/*`; documented honestly in `docs/architecture.md` §7 and
  `docs/security.md:67-70`. (Note: `CLAUDE.md` lists this as "unverified" — it is now
  verified **absent**.)
- **Why:** Repeated entropy is only dangerous for **crypto generated inside the VM**. In
  LambdaDoom, AWS terminates TLS at the endpoint, so the in-VM hop is plain HTTP/WS and this
  is **not exercised today**. The risk materializes only if a future capsule terminates TLS
  in-VM or generates keys/nonces.
- **Fix applied (docs):** No code action is correct for the current design (in-VM hop is
  plain; no entropy-sensitive crypto runs in the guest). Updated the `CLAUDE.md` note from
  "**unverified**" to "**ABSENT BY DESIGN (verified 2026-06-26)**" with the explicit condition
  under which a reseed/listener-bounce becomes required (any future in-VM TLS or key/nonce
  generation). `docs/architecture.md` §7 already states this.

### L4 — Release SHA256 sidecar is same-origin as the binary  `[FIXED: docs]`

- **What:** `deploy.sh` downloads `ldoom` from GitHub Releases and verifies it against a
  `.sha256` sidecar **downloaded from the same release**. An attacker who can replace the
  release asset can replace the matching sidecar, so the SHA256 check alone proves nothing
  about authenticity.
- **Where:** `deploy.sh:118-121` (download asset + sidecar from the same URL, then
  `verify_sha256`).
- **Why:** This is largely mitigated: `deploy.sh:57-71` also runs **`gh attestation verify`**
  (build provenance), which `release.yml:68-71` produces — that *is* a real
  cryptographic integrity control tied to the workflow identity. The SHA256 step is therefore
  a transport-integrity check, not an authenticity one.
- **Fix applied (docs):** Added a comment block above `verify_sha256` in `deploy.sh`
  spelling out that the sidecar is a transport-integrity check only (truncated/corrupt
  downloads), **not** an authenticity control, and that `verify_attestation` (GitHub build
  provenance bound to the workflow identity) is the cryptographic trust anchor. Confirmed
  attestation is already non-skippable for `latest` (skip requires a pinned
  `LAMBDADOOM_VERSION`, `deploy.sh:60-64`). No logic change needed.

### L5 — `uninstall.sh` empties and deletes account resources with broad `|| true` swallowing  `[FIXED]`

- **What:** `uninstall.sh` runs `aws s3 rm s3://$BUCKET --recursive` and
  `cloudformation delete-stack` with errors suppressed (`|| true`, `2>/dev/null`). `$BUCKET`
  is derived from a stack lookup; the recursive delete proceeds on whatever value it resolves.
- **Where:** `uninstall.sh:38-41` (recursive S3 delete), `uninstall.sh:36` (bucket from stack
  query), `uninstall.sh:5` (`set -uo pipefail` — note: **no `-e`**, so failures don't halt).
- **Why:** Bounded but worth noting: the bucket name comes from *your own* CloudFormation
  stack output, and there is a guard (`[ -n "$BUCKET" ] && [ "$BUCKET" != "None" ]`), so it
  will not run `rm` on an empty target. The risk is the broad error-swallowing: a partial
  failure (e.g. wrong region resolved, stack-delete blocked by a non-empty bucket) is hidden
  behind `|| true`, leaving the user thinking cleanup succeeded when resources (and billing)
  remain.
- **Fix applied:** Kept the existing non-empty bucket guard and kept `ldoom rm` best-effort
  (already-gone must stay non-fatal — deliberately did **not** add `set -e`, which would abort
  a re-run on the first already-deleted resource). The destructive AWS steps (`s3 rm`,
  `delete-stack`, `wait stack-delete-complete`) no longer swallow errors: each now reports a
  `warning:` and sets `FAILED=1`, and the script exits non-zero with a "verify in the AWS
  console (resources may still bill)" reminder if anything failed. Verified with `bash -n` and
  `shellcheck` (both clean).

---

## Verified-secure (checked and found NOT to be issues)

These are the attacks a reviewer would reach for first. I checked each against the code and
they are correctly defended — listed so the absence of a finding is intentional, not an
oversight.

- **CSRF / DNS-rebinding on control endpoints.** `/__lambdadoom/{state,suspend,resume}` drive
  the control plane with your AWS creds, but require: loopback `Host` **and** loopback
  `Origin` when present (`loopback_metadata_ok`, `is_loopback_authority`, `proxy.rs:524-553`),
  the HttpOnly + `SameSite=Strict` `ldoom_control` cookie (`proxy.rs:316-323,651-659`,
  `has_local_session`), and `POST` for the mutating actions (`proxy.rs:705-710`). A
  cross-origin, rebound, or blind local page gets 403. Covered by unit tests
  (`data_plane_metadata_rejects_foreign_origin`, `control_secret_cookie_must_match`).
- **Cross-origin keystroke injection into `/ldoom/input`.** The same loopback Host/Origin +
  session-secret gate applies to data-plane forwarding via `data_plane_rejection`
  (`proxy.rs:593-617`) and the `expected_forward_path` allowlist (`proxy.rs:577-591`), so a
  foreign page cannot open a WS to the input channel and type into your game.
- **Port smuggling (browser reaching :9000 or :5901).** `build_upstream_headers`
  (`proxy.rs:494-505`) and the WS path (`proxy.rs:351-358`) **`insert`** (replace, not append)
  `x-aws-proxy-auth` and `x-aws-proxy-port`, and the port is chosen server-side from the
  route table (`port_for`, `proxy.rs:105-112`). The browser cannot override the upstream port
  or token. Also the minted token's `allowedPorts` is scoped to 6901-6904.
- **Token leakage to the browser.** The JWE lives only in the proxy (`Upstream`,
  `proxy.rs:57-81`); it is injected server-side and never written into the page. The local
  `ldoom_control` cookie is stripped before forwarding upstream (`strip_control_cookie`,
  `proxy.rs:619-636`, tested).
- **Wake-on-traffic billing surprise.** A root GET while SUSPENDED is served the local Resume
  page instead of being forwarded (which would auto-resume and silently restart billing) —
  `handle_http:207-217`, gated on control-plane `current_state`.
- **IAM blast radius.** Build role is `s3:GetObject` on the artifact bucket + CloudWatch Logs
  only; execution role has **no** policies; bucket is private, AES256-encrypted, TLS-enforced
  (`DenyInsecureTransport`), with a 3-day lifecycle (`deploy/doom.yaml`). Least-privilege is
  real here.
- **Capsule supply-chain pinning.** Dockerfile SHA256-pins ffmpeg, noVNC, SDL2/mixer/net,
  Chocolate Doom, and the shareware WAD; `requirements.txt` is fully hash-pinned with
  `--require-hashes`. (The dead-wheel issue M2 is about *scope*, not pinning.)

## Considered, not ranked (informational)

- **Non-constant-time secret compare.** `cookie_has_control_secret` (`proxy.rs:839-846`) uses
  `==` on the 256-bit hex secret. A timing oracle requires a same-host attacker, who is
  explicitly out of scope (already owns your creds). Not worth a constant-time dep; noted only
  for completeness.
- **ffmpeg stderr forwarded to the browser** (`video_ws.py:84-89`). Diagnostic only, served
  to the single local user; no untrusted consumer. Not an issue under this threat model.
