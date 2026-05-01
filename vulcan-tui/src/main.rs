// Placeholder TUI binary. Full TUI extraction lands under GH issue
// #555 (Slice 4: Frontend extension binary + tool-result renderer).
// Until then, the working TUI continues to ship as the `vulcan` daemon
// binary's `chat` subcommand.
use vulcan_ext_snake as _;
use vulcan_ext_spinner_demo as _;
use vulcan_ext_todo as _;

fn main() -> anyhow::Result<()> {
    eprintln!("vulcan-tui: full TUI extraction is GH issue #555.");
    eprintln!("Use `vulcan` (or `vulcan chat`) for now.");
    Ok(())
}

#[cfg(test)]
fn frontend_capabilities_for_daemon() -> Vec<vulcan::extensions::FrontendCapability> {
    let mut out = Vec::new();
    for extension in vulcan_frontend_api::collect_registrations() {
        for cap in extension.frontend_capabilities() {
            let mapped = vulcan::extensions::FrontendCapability::parse(cap);
            if let Some(mapped) = mapped
                && !out.contains(&mapped)
            {
                out.push(mapped);
            }
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
        let descriptors = vulcan_frontend_api::collect_registrations()
            .into_iter()
            .map(
                |extension| vulcan_frontend_api::FrontendExtensionDescriptor {
                    id: extension.id().to_string(),
                    version: extension.version().to_string(),
                },
            )
            .collect::<Vec<_>>();
        assert!(descriptors.iter().any(|descriptor| descriptor.id == "todo"));
    }
}
