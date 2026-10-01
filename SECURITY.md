# Security policy

## Reporting a vulnerability

Report security issues privately by email to
[security@chainguard.dev](mailto:security@chainguard.dev). Do not open a
public issue or pull request for a suspected vulnerability.

challoc is a memory allocator, so a report is in scope when it shows any
of these:

- a heap inconsistency that challoc does not detect and abort on (an
  invalid, double or mis-sized free that is let through outside the
  windows `docs/hardening.md` documents);
- a way to make `malloc` return memory that overlaps a live block, the
  allocator's metadata, or an address the attacker chooses;
- a way to defeat a hardening feature that is on (guard slabs, `mseal`,
  `pac_pointers`, `zero_on_free`);
- a crash or misbehaviour reachable through the configuration sources
  (`MALLOC_CONF`, `CHALLOC_CONF`, `/etc/challoc.conf`, `malloc_conf`),
  in particular any that works across a setuid boundary.

To help triage, include:

- a description of the issue and its impact;
- steps to reproduce, or a proof of concept (a C program run under
  `LD_PRELOAD` is ideal);
- the commit, the architecture, the kernel version and the
  `Kernel and CPU features:` line of `malloc_stats_print` (or the output of
  `challoc_features()`);
- any known mitigation, and how you would like to be credited.

You can expect an acknowledgement within 72 hours and an open dialogue
until a fix is available. The full policy is the
[Chainguard Vulnerability Disclosure Policy](https://www.chainguard.dev/legal/inbound-vulnerability-disclosure-policy).

## Supported versions

challoc has no releases yet. Fixes go to the `main` branch.
