# Security policy

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository:
**[Report a vulnerability](https://github.com/nworks3d/THOR-memory-bible-for-AI/security/advisories/new)**.
It opens a private draft advisory that only you and the maintainer can see,
so nothing about the problem is public before a fix exists.

Private vulnerability reporting is not turned on for this repository yet. If
the link above refuses to open a draft, use the fallback instead: open a
regular [GitHub issue](https://github.com/nworks3d/THOR-memory-bible-for-AI/issues/new)
and add the `security` label (create it if it does not exist yet, or say
"security" plainly in the title). Do not paste secrets, real file paths from
your own machine, or anything else you would not want public into that issue
- describe the problem in general terms and the maintainer will follow up
privately for details.

You should get a response within a few days. This is a project maintained by
one person in their spare time, not a company with a security team - please
be patient.

## What is in scope

- The `thor2` Rust workspace: the event log, the write gate and its checks,
  the tool server, the code index, and the install/doctor/verify/sync
  programs.
- The install scripts (`install.ps1`, `install.sh`) and the checksum they
  verify the downloaded release against.
- The published container image (`ghcr.io/nworks3d/thor-mcp`).

Examples of what belongs here: a way to make the write gate accept something
it should refuse, a way to read or write another project's memory from one
that should not be able to, a way to make the install script run something
other than what it downloaded, or a crash that corrupts the event log instead
of refusing cleanly.

## What is out of scope

- The `thor2/eval/` directory. It holds one person's own measurement data,
  is excluded from the repository entirely (`.gitignore`), and is never part
  of a release or the published container.
- A vulnerability that lives entirely in a third-party dependency, with no
  path through THOR's own code - please report that to the dependency's own
  project instead. A dependency THOR bundles in a way that makes an
  otherwise-theoretical issue reachable is still in scope here.
- Denial of service against a service you run yourself with the optional
  network transport exposed on purpose (see below) - that is a configuration
  choice, not a vulnerability in THOR.

## How this is built to begin with

The default way of running THOR - a coding assistant talking to the `mcp`
program over standard input and output on your own machine - opens no
network port and asks for no key. There is nothing to attack from the
network in that mode, because there is no network in that mode.

The optional network transport (`mcp --http`) is different, and worth
understanding before you turn it on: **it carries no authentication of its
own.** It exists for one purpose - reaching your own memory from a second
machine, such as a NAS or a phone-facing session - and it is only ever safe
to run behind something else that does the authenticating for you, such as
an authenticating tunnel (Cloudflare Access and similar) or a strict
allowed-host list on a private network you control. Never put it on the open
internet by itself. `thor2/README.md`'s own section on writing to the store
from elsewhere says the same thing about the matching `sync` transport, which
shares a token for exactly this reason.

## Supported versions

THOR does not carry its own runtime version number (`mcp`/`serve` have no
`--version` flag) - a release is identified by its git tag and build log.
Only the latest tagged release is supported; there are no older lines
receiving separate security fixes.
