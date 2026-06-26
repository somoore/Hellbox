# Beyond DOOM: reusable MicroVM capsules

LambdaDoom is the acid test for serverless MicroVMs: if you can freeze and resume an
interactive game, you can freeze and resume an app, dev environment, agent sandbox, or thin
desktop session.

DOOM is useful because it exercises the hard parts at once:

- continuous GUI rendering
- audio
- low-latency keyboard input
- authenticated browser streaming
- suspend/resume of visible in-memory state
- a local proxy that hides the MicroVM data-plane token from the browser

A Flask demo proves the API works. LambdaDoom proves the primitive can feel like a live
machine.

## What stays the same

Most of the repo is reusable for another capsule:

- `ldoom build` still zips a Docker build context, uploads it to S3, and calls
  `CreateMicrovmImage`.
- `ldoom up`, `suspend`, `resume`, `down`, `rm`, and `ps` still drive the lifecycle.
- `ldoom open` still mints an auth token and runs the loopback proxy.
- The proxy still maps browser paths to internal MicroVM ports through `X-aws-proxy-port`.
- The ready hook still gates the image snapshot until the app is truly usable.

The replacement work is mostly inside `capsule/`.

## Replace the app

1. Keep the AWS hook responder on port `9000`.
2. Start your app from `capsule/rootfs/opt/capsule/start.sh` or a script it calls.
3. Keep a readiness gate that proves the app has rendered or is otherwise ready before
   `/ready` returns 200.
4. Expose the browser-facing services on internal ports and map them in `rs-cli/src/commands/open.rs`.
5. Keep all code native ARM64 unless you have validated another runtime across Graviton hosts.

For GUI apps, the current pattern is:

- `Xvnc :1` for a virtual display
- one streaming path for pixels
- one streaming path for audio
- one input path for keyboard/mouse events
- a browser page that connects to those WebSockets

For headless services, you can drop the display stack and make the proxy forward only your
HTTP/WebSocket app port.

## Snapshot correctly

The main failure mode is returning ready too early. A MicroVM image is snapshotted when the
ready hook returns 200. If the app has not drawn, connected to its backing store, generated
first-run state, or finished bootstrapping, every run resumes that broken moment.

Use an app-specific proof:

- GUI app: sample rendered pixels or hit a health endpoint that depends on the UI loop.
- Web app: require the HTTP server and critical background workers to answer.
- Agent sandbox: require tools, policy files, and workspace mounts to be loaded.
- Dev environment: require the editor/server process and workspace index to be ready.

## Keep the security model

Do not give the browser the `X-aws-proxy-auth` token. Keep the Fork B loopback proxy:

- browser to proxy: plain HTTP/WS on `127.0.0.1`
- proxy to MicroVM endpoint: HTTPS/WSS with `X-aws-proxy-auth` and `X-aws-proxy-port`
- browser control endpoints: `/__lambdadoom/*` with loopback Host/Origin checks and the
  per-session `ldoom_control` cookie

If your new capsule handles secrets, assume suspend/resume freezes process memory. Reseed or
rotate anything that must not replay after resume.

## Keep the supply chain boring

Every external artifact in the capsule should be pinned by version and SHA256. Prefer:

- pinned release tags, not `latest`
- immutable commit URLs, not branch URLs
- release checksum files for local binaries
- GitHub artifact attestations for release binaries

That is why the current Dockerfile looks verbose: the image build is running in the user's
AWS account, so all network downloads need to be auditable.

## Good next capsules

- A browser-hosted IDE or dev shell that resumes with terminals and editor state intact.
- A single-user desktop app streamed to a tab.
- An agent sandbox with expensive warm state and resumable tool context.
- A GUI test runner that freezes at failure for inspection.
- A thin-client internal tool where users pay only while actively connected.
