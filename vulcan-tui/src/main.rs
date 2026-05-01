// Placeholder TUI binary. Full TUI extraction lands under GH issue
// #555 (Slice 4: Frontend extension binary + tool-result renderer).
// Until then, the working TUI continues to ship as the `vulcan` daemon
// binary's `chat` subcommand.

fn main() -> anyhow::Result<()> {
    eprintln!("vulcan-tui: full TUI extraction is GH issue #555.");
    eprintln!("Use `vulcan` (or `vulcan chat`) for now.");
    Ok(())
}
