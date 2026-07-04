# Security Policy

## Supported versions

Strenor is in **alpha** (`0.0.x`). Security fixes are applied to the latest
published version. Once `1.0` ships, this section will list supported ranges.

## Reporting a vulnerability

Please report security issues **privately**, not in a public issue:

- Open a [GitHub Security Advisory](https://github.com/Brashkie/strenor/security/advisories/new), or
- Contact the maintainer through the profile on
  [github.com/Brashkie](https://github.com/Brashkie).

Include a description, reproduction steps, affected version, and impact. You'll
get an acknowledgement as soon as possible, and coordinated disclosure once a fix
is available.

## Scope

Strenor is an in-process, embedded store with no network surface by default, so
the attack surface is small. Areas of interest: snapshot parsing
(`load`), native memory handling in the Rust core, and codec deserialization.
