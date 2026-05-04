# Renderer LaTeX Decision

Issue: #589

## Decision

Defer full LaTeX document rendering from Renderer Pipeline v1.

Vulcan should keep Markdown math as the default TeX-adjacent behavior and prefer
Typst previews before introducing a full LaTeX document pipeline. If full LaTeX
is added later, the first implementation should be a sidecar/subprocess worker
behind explicit opt-in config and a default-off `render-latex` feature.

## Comparison

| Approach | Default build impact | Runtime/security profile | Fit |
| --- | --- | --- | --- |
| Embedded `tectonic` crate | High | Pulls TeX engine complexity into the Rust binary | Defer |
| External `tectonic`/`latexmk` subprocess | None when feature/config disabled | Isolatable with timeout, temp dir, scrubbed env, and output caps | Best later option |
| Defer full LaTeX | None | No new renderer attack surface | Best for v1 |

## Later Implementation Contract

A future LaTeX renderer should:

- Be disabled by default and gated behind `render-latex`.
- Run outside the TUI draw path in a background worker or sidecar process.
- Use a cache under `vulcan_home()/render-cache/latex`.
- Key cache entries by source hash, engine, executable version, output format,
  renderer version, and relevant options.
- Compile in a temporary working directory under the cache root.
- Invoke executables directly without a shell.
- Scrub environment variables and pass an explicit executable path from config.
- Disable shell escape and restrict include roots where the chosen engine allows
  it.
- Enforce timeout/kill behavior and cap stdout/stderr/error payload sizes.
- Render source text by default, and when enabled show generated artifact paths
  or preview placeholders without assuming terminal image support.

## Runtime Requirements If Enabled Later

An external sidecar route should document at least:

- Supported executable: `tectonic` CLI first; `latexmk` only when a TeXLive-like
  install is intentionally supported.
- Required config: executable path, timeout, output format, include roots, and
  cache size/retention.
- Failure UX: source fallback plus a concise error and generated artifact path
  when available.
- Security caveat: user-provided `.tex` is active document code, not passive
  Markdown text.

## v1 Outcome

Renderer Pipeline v1 does not add a `render-latex` feature or LaTeX dependency.
Default `cargo test` and default builds remain unaffected by full document TeX
support.
