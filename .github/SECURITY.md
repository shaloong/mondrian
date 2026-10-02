# Security Policy / 安全政策

## Report privately / 私密报告

Send suspected vulnerabilities to **contact@shaloong.com**, with the subject
`[Mondrian Security]` and a short description. Do not open a public issue or PR
containing an undisclosed vulnerability, exploit, private media, credentials,
or personal information. English and Chinese reports are welcome.

请通过 **contact@shaloong.com** 私密报告疑似漏洞，主题使用
`[Mondrian Security]`。不要在公开 Issue 或 PR 中披露未公开漏洞、利用代码、
私人素材、凭证或个人信息。支持中文和英文报告。

If the repository's [security page](https://github.com/shaloong/mondrian/security)
offers **Report a vulnerability**, GitHub private reporting is also available.
Email remains the fallback if that option is absent or unavailable. A policy
file alone does not enable GitHub private reporting.

Include the affected release or commit, OS and relevant native-library versions,
reproduction steps, expected and observed behavior, and the potential impact.
Use a small synthetic reproducer where possible. Ask for a suitable private
transfer channel before sending large or sensitive files.

## Versions and scope / 版本与范围

Reports are welcome for all versions. Fix development targets the current
`develop` branch and the latest public release; maintainers determine whether
a release fix or an upgrade is appropriate. There is no published LTS or older
release backport commitment. Version `0.2.0` in Cargo metadata is not proof of
a public release or a supported-version announcement.

The project covers Mondrian's editor, project/interchange parsing, media and
image processing, native bridges, build automation, and distributed runtime.
Report vulnerabilities in bundled third-party components when Mondrian users
may be affected; maintainers coordinate with upstream rather than silently
excluding dependencies. Ordinary crashes and correctness bugs can use the
public tracker when they do not disclose a suspected security problem. When
unsure, report privately.

See the [security design and evidence](../docs/security/design.md) for current
boundaries and limitations. That document is not an exclusion list.

## Response and coordinated disclosure / 响应与协调披露

Maintainers aim to acknowledge reports within **3 business days**, and must
provide an initial response within **14 calendar days**. These are response
requirements, not a guarantee of a fix by that date. If no acknowledgment
arrives, resend to the same address with `[Follow-up]` in the subject.

1. Assign a handler and acknowledge receipt privately.
2. Reproduce the issue, assess affected versions and impact, and check related
   paths for the same failure. Explain the outcome to the reporter, including
   the reason if it is not considered a vulnerability.
3. Prepare a fix, a regression test, and mitigation or upgrade instructions.
   Keep investigation and exploit details in restricted storage; a public
   branch or Actions artifact is not a private workspace.
4. Agree on a disclosure date with the reporter and relevant upstream projects.
   Give progress updates at least every 14 calendar days while a confirmed
   report is open. Active exploitation or a public disclosure requires
   accelerated handling and may justify immediate mitigation advice.
5. Publish the fix and advisory together where practical. Include affected and
   fixed versions, impact, mitigation, verification guidance, and a GHSA/CVE
   when assigned. Credit reporters with their consent; honor anonymity.

Critical issues receive immediate prioritization. Publicly known medium or
higher severity vulnerabilities must not remain unpatched for more than
60 days. If a fix is blocked, escalate to repository administrators and publish
safe mitigation guidance; an explanation alone does not satisfy that limit.

The process follows the public-reporting and response requirements in the
[OpenSSF Passing criteria](https://www.bestpractices.dev/en/criteria/0) and draws
on [Scorecard's reporting workflow](https://github.com/ossf/scorecard/blob/main/SECURITY.md)
and [curl's coordinated disclosure process](https://curl.se/dev/vuln-disclosure.html).
Their contacts, service commitments, and project-specific exclusions do not
apply to Mondrian.
