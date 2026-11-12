# Security Policy

Report vulnerabilities through
[GitHub Security Advisories](https://github.com/zvuk/kithara/security/advisories/new),
not public issues. We acknowledge a report within 48 hours and aim to ship a
fix or mitigation for a critical issue within 7 days.

Fixes go to the latest release only. The attack surface is the network layer
(HTTP, HLS playlists and segments), AES-128 segment decryption, and media
parsing in the decoders.
