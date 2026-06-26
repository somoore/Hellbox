<div align="center">

# LambdaDoom

### DOOM, running inside an AWS Lambda MicroVM, streamed to your browser.

Suspend it mid firefight and the compute bill stops. Resume it and you are back on the exact
frame, same demon mid lunge, same health, same ammo.

</div>

---

```bash
ldoom open   # a browser tab opens. it's DOOM. it's running in the cloud.
```

LambdaDoom runs the original DOOM on a virtual machine in AWS and streams the video, audio,
and your keypresses to a browser tab. Nothing runs on your laptop but the tab. It deploys into
your own AWS account with one command, and one command removes it.

## What is an AWS Lambda MicroVM?

A new serverless building block that runs your code inside a full virtual machine: real VM
isolation, near instant launch, and the ability to freeze a running machine and resume it
later with its state intact, all with no servers to manage. It is powered by Firecracker, the
same virtualization behind AWS Lambda. AWS launched it in June 2026:
[the announcement](https://aws.amazon.com/blogs/aws/run-isolated-sandboxes-with-full-lifecycle-control-aws-lambda-introduces-microvms/).

The piece that makes LambdaDoom work is suspend and resume. AWS snapshots the live memory of
the VM, stops charging for compute, and restores it on demand. Freezing a live DOOM fight is
the most direct way I found to feel what that primitive actually does.

## Quickstart

You need the [AWS CLI](https://aws.amazon.com/cli/) configured with credentials. Then:

```bash
git clone https://github.com/somoore/LambdaDoom
cd LambdaDoom
./deploy.sh
```

`deploy.sh` deploys the AWS prerequisites, downloads the prebuilt `ldoom` CLI for your system,
builds the MicroVM image, launches it, and opens DOOM in your browser. In the tab: click the
speaker icon for sound, click the game, and play (`W A S D` to move, `Ctrl` to fire, `Space`
to open doors). Hit Suspend when you step away to stop the compute bill, and Resume to pick up
where you left off.

> **Cost:** a suspended VM is roughly cents per month, and a running, streamed session is
> roughly $0.19 per hour. Suspend when you walk away, or run `./uninstall.sh` to remove
> everything (the VM, the image, the stack, and all local state).

## How it works

```
your machine (thin client)              AWS
+--------------------------+            +--------------------------------------+
|  ldoom (Rust CLI)        |  SigV4/SDK |  Lambda MicroVMs control plane        |
|  build up open suspend.. |----------->|  create-image . run . token . suspend |
|  loopback proxy          |  injects X-aws-proxy-auth header
|  127.0.0.1:6080  --------+---- WSS --->  <id>.lambda-microvm.<region>.on.aws
|  browser tab             |            |  MicroVM (ARM64 Firecracker)          |
+--------------------------+            |   Chocolate Doom -> H.264 + Opus      |
                                        +--------------------------------------+
```

A small Rust CLI (`ldoom`) drives the lifecycle. Inside the VM, native ARM Chocolate Doom
renders into a headless X server; an encoder streams it as H.264 with Opus audio over
WebSockets, and the browser decodes it with WebCodecs. The MicroVM endpoint needs an auth
header that browsers cannot set, so `ldoom open` runs a tiny loopback proxy that injects it.

Full design is in [docs/architecture.md](docs/architecture.md), the security model is in
[docs/security.md](docs/security.md), and the verified MicroVMs API facts are in
[docs/microvm-ground-truth.md](docs/microvm-ground-truth.md).

<details>
<summary><b>Configuration</b> (environment variables)</summary>

| Variable | Default | What it does |
|---|---|---|
| `AWS_REGION` | `us-east-1` | Region to deploy into (any region with Lambda MicroVMs works). |
| `LAMBDADOOM_NAME` | `doom` | Capsule name (the image and the MicroVM). |
| `LAMBDADOOM_STACK` | `LambdaDoom` | CloudFormation stack name. |
| `LAMBDADOOM_VERSION` | latest release | Pin the `ldoom` binary to a specific release tag. |
| `LAMBDADOOM_HOME` | `~/.lambdadoom` | Where config, state, and the binary live. |
| `LDOOM_BIN` | none | Use a local `ldoom` binary instead of downloading one. |

</details>

<details>
<summary><b>One-click prerequisites</b> (Launch Stack)</summary>

This button uses CloudFormation to create only the prerequisites: the S3 build bucket and the
two IAM roles. After it finishes, copy the stack outputs into `~/.lambdadoom/config.toml` and
run `ldoom build`, `ldoom up`, `ldoom open`. For the full setup in one step (prerequisites,
binary, build, launch, and open), use `./deploy.sh` instead.

[![Launch Stack](https://s3.amazonaws.com/cloudformation-examples/cloudformation-launch-stack.png)](https://console.aws.amazon.com/cloudformation/home?region=us-east-1#/stacks/create/review?templateURL=https://lambdadoom-launch-932930471665.s3.amazonaws.com/doom.yaml&stackName=LambdaDoom)

</details>

<details>
<summary><b>Manual control</b> (the <code>ldoom</code> CLI)</summary>

`deploy.sh` wraps one CloudFormation stack ([`deploy/doom.yaml`](deploy/doom.yaml)) and the
`ldoom` CLI. To drive them yourself, deploy the stack from the template file, copy its outputs
into `~/.lambdadoom/config.toml`, then run the lifecycle:

```bash
# one-time: create the S3 bucket + IAM roles from the local template
aws cloudformation deploy --region us-east-1 --stack-name LambdaDoom \
  --template-file deploy/doom.yaml --capabilities CAPABILITY_IAM

ldoom build      # zip capsule -> S3 -> build image (compiles engine, fetches WAD) -> CREATED
ldoom up         # launch a MicroVM from the image          (PENDING -> RUNNING)
ldoom open       # mint a token, open the browser tab, play DOOM
ldoom suspend    # freeze the VM (compute billing stops)
ldoom resume     # thaw on the exact frame
ldoom down       # terminate the MicroVM (keeps the image so up can relaunch)
ldoom rm         # full teardown: terminate and delete the image
ldoom ps         # list capsules and their state
```

`ldoom` is the prebuilt binary `deploy.sh` downloads. To build it yourself instead, the source
is in [`rs-cli/`](rs-cli/): `cd rs-cli && make release`.

</details>

## Legal

LambdaDoom ships **only** the **shareware** `DOOM1.WAD` plus **GPLv2 Chocolate Doom**, both
fetched at build time and never committed to this repo. It never ships the retail `DOOM.WAD` or
`DOOM2.WAD`. DOOM and the DOOM WADs are trademarks and property of their respective owners.

This is an independent, unofficial project, not for sale, and not affiliated with or endorsed
by AWS, Amazon.com, or id Software. AWS, AWS Lambda, and Firecracker are trademarks of
Amazon.com, Inc. or its affiliates, used here only to describe what LambdaDoom runs on.
LambdaDoom runs on AWS services in your own account, under your own agreement with AWS, and you
are responsible for any charges it incurs.
