# Security Policy

Thanks for helping keep suzume-sql and its users safe.

suzume-sql is a terminal client for databases. It reads and stores **database connection details, including credentials**, so security reports are taken seriously even for a small project.

## Reporting a vulnerability

**Please do not open a public issue for security problems** — that exposes the flaw before it can be fixed.

Instead, report privately:

- **GitHub private vulnerability reporting** (preferred): open the [Security tab](https://github.com/Alexrp02/suzume-sql/security) and click *Report a vulnerability*.

Please include:

- a description of the issue and its impact,
- steps to reproduce (or a proof of concept),
- the suzume-sql version (`suzume --version`) or commit, your OS, and the database engine involved.

**Do not include real credentials** in your report — use redacted or dummy values.

## What to expect

This is a maintainer-time project, so timelines are best effort:

- I aim to acknowledge your report within **7 days**.
- Once confirmed, I'll work on a fix and keep you updated on progress.
- I'll credit you in the release notes when the fix ships, unless you'd prefer to stay anonymous.

Please give me a reasonable chance to release a fix before disclosing the issue publicly.

## Supported versions

suzume-sql is pre-1.0 and moves fast. Only the **latest release** (and current `main`) receives security fixes; there are no backports to older versions.

## Scope

Examples of things worth reporting:

- credentials or connection strings being logged, written to disk (outside of the default config folder), or copied to the clipboard unintentionally,
- unsafe handling of untrusted database contents (e.g. terminal escape injection when rendering cell values),
- any way a crafted config file, connection string, or query result leads to code execution or data exfiltration.

Vulnerabilities in the underlying databases or in third-party dependencies should be reported to their respective projects, though a heads-up here is always welcome.
