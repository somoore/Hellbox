# Security Policy

LambdaDoom is a single-user, own-AWS-account demo. The full threat model — trust
boundaries, what protects you, and the deliberate non-goals — is documented in
[docs/security.md](docs/security.md). Read that first; it is the authoritative description
of the security posture.

## Reporting a vulnerability

This is a personal demo, not a hosted service. If you find a security issue, please **do not
post a public exploit**. Instead:

- Open a [private security advisory](https://github.com/somoore/LambdaDoom/security/advisories/new), or
- Open a regular issue for low-risk findings, or
- Contact the repository owner directly.

There is no bug-bounty program and no SLA, but reports are appreciated and will be reviewed.

## Supported versions

Only the latest release and `main` are maintained. There are no backported security fixes for
older tags.
