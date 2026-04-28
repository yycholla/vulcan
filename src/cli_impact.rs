//! YYC-218 / YYC-189: `vulcan impact <file>` driver.

use anyhow::Result;
use std::path::Path;

use crate::artifact::{Artifact, ArtifactKind, SqliteArtifactStore};
use crate::impact::{generate_for_file, render_markdown};

pub async fn run(target: &Path, save: bool) -> Result<()> {
    let workspace = std::env::current_dir()?;
    let report = generate_for_file(&workspace, target)?;
    let md = render_markdown(&report);
    print!("{md}");
    if save {
        match SqliteArtifactStore::try_new() {
            Ok(store) => {
                let art = Artifact::inline_text(ArtifactKind::Report, md)
                    .with_source("impact")
                    .with_title(format!("Impact: {}", target.display()));
                let id = art.id;
                if let Err(e) = ArtifactStoreExt::create(&store, &art) {
                    eprintln!("artifact persist failed: {e}");
                } else {
                    eprintln!("impact report persisted as artifact {id}");
                }
            }
            Err(e) => eprintln!("artifact store unavailable: {e}"),
        }
    }
    Ok(())
}

trait ArtifactStoreExt {
    fn create(&self, art: &Artifact) -> Result<()>;
}

impl ArtifactStoreExt for SqliteArtifactStore {
    fn create(&self, art: &Artifact) -> Result<()> {
        crate::artifact::ArtifactStore::create(self, art)
    }
}
