# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately, by either:

- GitHub's private vulnerability reporting on this repository
  (**Security → Report a vulnerability**), or
- email to **security@haarness.dev**.

Include what you need us to reproduce it: the Haarness version (`haarness --version`),
the platform, the configuration involved (redact secrets), and the steps or a proof of
concept. Tell us how you would like to be credited.

What to expect:

| Step | Target |
| --- | --- |
| Acknowledgement of your report | 3 business days |
| First assessment (accepted, needs info, or not a vulnerability) | 10 business days |
| Fix or mitigation for accepted high/critical issues | 90 days, usually much sooner |

We will keep you informed, agree a disclosure date with you, and credit you in the
advisory unless you ask us not to. We will not take legal action against research done
in good faith within this policy: stay on your own installations and accounts, do not
access or modify other people's data, and give us reasonable time to fix before disclosure.

## Supported versions

Haarness is pre-1.0. Security fixes land on the default branch and ship in the next
release; only the latest release is supported.

## Scope

In scope: the `haarness` executable (CLI, TUI, server, ACP agent), Haarness Desktop, the
SDKs in `sdks/`, the install scripts, and the release artifacts built from this repository.

Things that are **by design** and not vulnerabilities on their own:

- Haarness permissions are application-level policy, **not an operating-system sandbox**.
  A tool you approved, or allowed by rule, runs with your user's privileges. A way to run a
  tool *without* the approval the policy requires is in scope.
- A model or MCP server you configured returning malicious content. Prompt-injection that
  bypasses a permission check or an approval prompt is in scope.
- Exposing the server beyond loopback without authentication after explicitly opting in.

Haarness Cloud (the hosted service at haarness.dev) has its own disclosure policy at
<https://haarness.dev/security>.
