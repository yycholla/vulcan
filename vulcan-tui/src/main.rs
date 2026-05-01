// Placeholder TUI binary. Full TUI extraction lands under GH issue
// #555 (Slice 4: Frontend extension binary + tool-result renderer).
// Until then, the working TUI continues to ship as the `vulcan` daemon
// binary's `chat` subcommand.
use vulcan_ext_spinner_demo as _;
use vulcan_ext_todo as _;

fn main() -> anyhow::Result<()> {
    let caps = frontend_capabilities_for_daemon();
    let extensions = vulcan_frontend_api::collect_frontend_descriptors();
    eprintln!("vulcan-tui: full TUI extraction is GH issue #555.");
    eprintln!("frontend capabilities: {}", caps.len());
    eprintln!("frontend extensions: {}", extensions.len());
    eprintln!("Use `vulcan` (or `vulcan chat`) for now.");
    Ok(())
}

fn frontend_capabilities_for_daemon() -> Vec<vulcan::extensions::FrontendCapability> {
    let mut out = Vec::new();
    for cap in vulcan_frontend_api::collect_frontend_capabilities() {
        let mapped = match cap {
            "text_io" => Some(vulcan::extensions::FrontendCapability::TextIo),
            "rich_text" => Some(vulcan::extensions::FrontendCapability::RichText),
            "cell_canvas" => Some(vulcan::extensions::FrontendCapability::CellCanvas),
            "raw_input" => Some(vulcan::extensions::FrontendCapability::RawInput),
            "status_widgets" => Some(vulcan::extensions::FrontendCapability::StatusWidgets),
            _ => None,
        };
        if let Some(mapped) = mapped
            && !out.contains(&mapped)
        {
            out.push(mapped);
        }
    }
    if !out.contains(&vulcan::extensions::FrontendCapability::TextIo) {
        out.push(vulcan::extensions::FrontendCapability::TextIo);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn collects_frontend_capabilities_for_daemon_connection() {
        let caps = super::frontend_capabilities_for_daemon();
        assert!(caps.contains(&vulcan::extensions::FrontendCapability::TextIo));
        assert!(caps.contains(&vulcan::extensions::FrontendCapability::RichText));
    }

    #[test]
    fn collects_frontend_extension_descriptors_for_handshake() {
        let descriptors = vulcan_frontend_api::collect_frontend_descriptors();
        assert!(descriptors.iter().any(|descriptor| descriptor.id == "todo"));
    }
}
