# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it through [GitHub Security Advisories](https://github.com/zvuk/kithara/security/advisories/new).

**Please do not open a public issue for security vulnerabilities.**

We will acknowledge receipt within 48 hours and aim to provide a fix or mitigation within 7 days for critical issues.

## Security Considerations

kithara handles HTTP networking, HLS streaming, and AES-128-CBC decryption. We take security seriously and recommend:

- Keeping dependencies up to date
- Using the latest supported version
- Reviewing the [RustSec Advisory Database](https://rustsec.org/) for known vulnerabilities in dependencies
