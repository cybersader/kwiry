# Security policy

## Supported versions

Kwiry is pre-1.0 software. Security fixes target the newest published release,
including the current beta line. Older versions do not receive separate
backports; report vulnerabilities in them when the newest release is also
affected. This policy does not expand the supported profiles or field-acceptance
claims in [ROADMAP.md](ROADMAP.md).

## Report a vulnerability privately

Use [GitHub private vulnerability reporting](https://github.com/cybersader/kwir/security/advisories/new)
(**Security → Report a vulnerability**) when the form is available. The owner must
enable private vulnerability reporting in repository settings before this route
can accept reports. If the form is unavailable, open a public issue requesting a
private reporting channel **without any vulnerability details**; wait for a
private channel before sharing the report.

Do not open a public issue containing exploit details, secrets or private data.
Include affected versions, impact and minimal reproduction steps. Send no live
credentials, private vault content or diagnostic exports containing personal
information. We aim to acknowledge private reports within seven days and agree
on a coordinated disclosure timeline after triage; this is not a guaranteed fix
SLA.
