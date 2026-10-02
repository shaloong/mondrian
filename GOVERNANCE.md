# Project Governance

Mondrian currently uses a maintainer-led model. Contributors propose changes
through public issues and pull requests; maintainers decide technical direction,
merge readiness, and release readiness. Significant domain or architecture
decisions are recorded in [CONTEXT.md](CONTEXT.md),
[architecture documents](docs/architecture/overview.md), and
[ADRs](docs/adr/0001-synthetic-clock-on-audio-device-loss.md).

## Responsibilities and decision making

| Role | Responsibility | Current contact or evidence |
| --- | --- | --- |
| Contributors | Explain changes, verify results, disclose provenance and licensing, respond to review | PR author and commit attribution |
| Reviewers | Check behavior, tests, security boundaries and compatibility; record review conclusions | Named GitHub reviews on each PR |
| Maintainers / repository administrators | Triage, merge, manage access, assign security and conduct handlers, approve releases | contact@shaloong.com; a confirmed public administrator/backup roster is still needed |
| Licensing contact | Handle commercial licensing and contribution-authority questions | Shaloong contact and licensor identified in [LICENSE](LICENSE) |

The public contribution history currently identifies
[@shalomwang](https://github.com/shalomwang) as the primary contributor.
Contribution history alone does not establish repository permissions, a
security-handler appointment, or backup release authority. Do not infer those
roles from authorship or list unconfirmed people as code owners.

For substantial changes, open an issue first and describe the user problem,
alternatives, compatibility impact, and validation. Seek consensus through the
public discussion. Maintainers record the decision and reasons; disagreement
can be escalated by linking the discussion in a follow-up issue. Vulnerability,
conduct, credential, and personal-data matters use private channels instead.

## Contribution and release authority

External changes target **develop**. **main** accepts promotion PRs from this
repository's **develop** branch. Both branches use PRs and CLA checks. See
[contributing](docs/CONTRIBUTING.md) and
[review requirements](docs/dev/code-review.md). Repository administrators must
verify live branch rules independently of written policy: require applicable CI
and CLA checks, retain deletion/force-push protection, and require independent
approval when the reviewer roster supports it. Enable private vulnerability and
content reporting, assign handlers, and verify alert routing. Review access and
backup readiness quarterly.

Release maintainers review CI, qualification evidence, dependency findings,
license notices, human-readable release notes, and security advisories before
publishing. A successful build, a tag, or an automatically created draft does
not authorize a public release. Present distribution qualification is described
in [the contribution guide](docs/CONTRIBUTING.md).

## Access and continuity

Repository write access, private-report access, and release authority require
2FA. Prefer passkeys/security keys or TOTP. Grant only the permissions needed,
review access when roles change, and revoke access when a maintainer leaves.
Store recovery material privately; never commit credentials or recovery codes.

The project must establish a second authorized human who can triage, merge,
access private reports, and release within one week if the primary maintainer
becomes unavailable. Confirm that person's consent, permissions, recovery
process, and ability to execute the documented release workflow. Exercise the
handover periodically and record only non-sensitive results publicly.

**This continuity arrangement is not yet evidenced.** This document does not
claim a bus factor of two, an independent appeal panel, or two unaffiliated
significant contributors. Bots and AI agents are tools and do not satisfy
human-maintainer or independent-review requirements.

New maintainers should demonstrate sustained contributions and review work,
accept these responsibilities, and receive an explicit public appointment from
the current maintainers. Update the roster and access controls together.

This small-team model borrows explicit responsibilities and documented decisions
from [Kubernetes governance](https://github.com/kubernetes/community/blob/main/governance.md).
It does not imply a steering committee or foundation affiliation.
