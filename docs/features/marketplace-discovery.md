---
title: Marketplace & Discovery
type: feature
created: 2026-05-14
tags: [extensions, marketplace, repo, discovery]
---

# Marketplace & Discovery

Make the extension store a living, self-sustaining ecosystem.

## Recommendations

- **Project-aware suggestions**: Recommend extensions based on project language, detected frameworks, and past installs (e.g., "Python project → ruff/pytest extensions").
- **Similar projects**: Show extensions used by similar repos/orgs (opt-in telemetry).

## Dependency Resolution

- **Versioned deps**: Resolve extension-to-extension and library dependencies, warn on conflicts.
- **Platform constraints**: Filter incompatible extensions by OS/architecture/runtime.
- **Semantic upgrades**: Safe minor/patch upgrades, semver-aware breaking-change warnings.

## Usage Analytics & Reputation (opt-in)

- **Download counts & popularity**: Surface widely used and trusted extensions.
- **Ratings & reviews**: Community feedback and publisher responses.
- **Compatibility badges**: "Works with Vulcan >= 0.4.0".

## Monetization & Sponsorship (optional)

- **Paid extensions**: Transparent pricing, license checks, and trial workflows.
- **Sponsorships**: Tip jars and sponsor links; verified publisher badges.

## Verified Publishers & Badges

- **Org/person verification**: Proof of ownership of GitHub repo or domain.
- **Curated collections**: Official and community-curated lists (e.g., "Security", "Data engineering").
- **Automated scans**: Display scan status (passed/failed) for each published extension.

---

## Example: Repo Index Entry with Analytics

```json
{
  "id": "memory@redis",
  "name": "Redis Memory Backend",
  "version": "1.0.2",
  "publisher": { "id": "acme", "verified": true },
  "downloads_last_30d": 3124,
  "rating": 4.7,
  "scans": { "clamav": "clean", "static": "passed" },
  "compatibility": { "vulcan": ">=0.4.0" },
  "download_url": "https://repo.vulcan.dev/packages/memory_redis-1.0.2.vpk",
  "checksum": "sha256:...",
  "signature": "base64:..."
}
```
