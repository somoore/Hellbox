# LambdaDoom — Security & Architecture Review

**Date:** 2026-06-26
**Scope:** Rust CLI (`rs-cli/`), the loopback proxy, the capsule runtime (`capsule/`),
infra (`deploy/doom.yaml`), and the deploy/release supply chain.
**Method:** Full manual read of every source file, plus `cargo deny check advisories`
and an import audit of the capsule scripts.

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

---

## MEDIUM

### M1 — CI has no automated vulnerability (CVE/RUSTSEC) gate

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
- **Fix:** Add an `[advisories]` section to `deny.toml` and run advisories in CI:
  ```yaml
  # ci.yml — extend the existing cargo-deny job or add a step
  command-arguments: check advisories licenses
  ```
  Or add a dedicated `cargo audit` step. Pin to a periodic schedule too (`on: schedule`) so a
  newly-disclosed CVE in an unchanged lockfile is surfaced without a push.

### M2 — Orphaned Python wheels enlarge the capsule attack surface

- **What:** `capsule/requirements.txt` hash-pins wheels that **no capsule code imports**:
  `redis`, `jwcrypto`, `cryptography` (and their transitive `cffi`/`pycparser`). The only
  things the capsule actually imports are `websockets` and `python-xlib` (Xlib); `numpy` and
  `requests` come in transitively via `websockify`. JWE/crypto is handled in **Rust** now, so
  `jwcrypto`/`cryptography` are leftovers from the `shrink-wrap` spike.
- **Where:** `capsule/requirements.txt:9,13,21` (`cryptography`, `jwcrypto`, `redis`);
  verified against `grep -rEi '^\s*(import|from)'` over `capsule/rootfs/` — `redis`,
  `jwcrypto`, `cryptography` appear nowhere outside `requirements.txt`.
- **Why:** Every package installed into the image is code that ships in the snapshot and
  counts toward the supply-chain/CVE surface for no functional benefit. `redis` in particular
  is wholly unexplained (it is not a `websockify` dependency).
- **Fix:** Drop `redis`, `jwcrypto`, `cryptography`, `cffi`, `pycparser` from
  `requirements.txt` unless a build step needs them. Verify the image still builds and DOOM
  still renders (the render gate in `start.sh` will catch a regression). Keep the file to
  exactly: `websockets`, `python-xlib`, `websockify`, and websockify's real transitive deps
  (`numpy`, `requests`, `certifi`, `charset-normalizer`, `idna`, `urllib3`, `six`,
  `typing-extensions`).

---

## LOW

### L1 — In-VM stream services bind `0.0.0.0` with no per-service authentication

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
- **Fix:** Low-effort hardening: bind the stream services to `127.0.0.1` and have the AWS
  ingress reach them via loopback if the platform allows; or add a shared-secret check on the
  WS handshake. At minimum, document that port-scoping in the minted token (currently
  6901-6904) is the load-bearing control and must never be widened to include 5901/9000.

### L2 — Default egress is the public internet

- **What:** Network connectors are intentionally omitted, so the MicroVM gets the
  Lambda-managed default of **`INTERNET_EGRESS`**. The capsule does not need outbound
  internet at runtime (the WAD and engine are baked at build time).
- **Where:** `up.rs:48-53` (connectors only set when non-empty); `config.rs:23-25` (default
  empty); documented in `docs/security.md:71-73`.
- **Why:** A compromised in-VM process (e.g. via a malicious WAD or a stream-service bug)
  could exfiltrate or beacon outbound. Documented as a non-goal, so this is informational
  hardening.
- **Fix:** If the platform exposes an egress connector that denies all outbound, wire it in
  for the runtime VM. Otherwise leave the documented note; it is an accepted trade-off for a
  single-user demo.

### L3 — No CSPRNG reseed on resume (documented, currently not exercised)

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
- **Fix:** No action needed for the current design. Before any future capsule does in-VM
  crypto, add a `resume`-hook step that reseeds `/dev/urandom` (e.g. writes fresh entropy)
  and bounces the affected listener. Update the `CLAUDE.md` note from "unverified" to
  "absent by design; required if in-VM TLS is added."

### L4 — Release SHA256 sidecar is same-origin as the binary

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
- **Fix:** Mostly a docs/clarity fix. Keep attestation as the load-bearing control and make
  it **non-skippable for `latest`** (it already requires a pinned version to skip — good).
  Consider noting in the script comments that the sidecar guards against truncated downloads,
  not tampering, and that attestation is the trust anchor. Building from source
  (`LDOOM_BIN`) remains the strongest path.

### L5 — `uninstall.sh` empties and deletes account resources with broad `|| true` swallowing

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
- **Fix:** Keep the existing non-empty guard. Surface failures instead of swallowing them:
  drop blanket `2>/dev/null || true` on the delete-stack path and report the real error, or
  print a final "verify in console" reminder. Consider confirming the bucket name belongs to
  the stack (it already does via the query) before the recursive `rm`.

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
