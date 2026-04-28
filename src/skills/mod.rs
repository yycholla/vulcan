//! YYC-243: spec-compliant Agent Skills loader (agentskills.io / Anthropic
//! SKILL.md format).
//!
//! Layout per skill: `<root>/<name>/SKILL.md` with YAML frontmatter
//! (`name`, `description`, optional `version`, `license`, `allowed-tools`)
//! followed by a markdown body. Optional `scripts/`, `references/`, and
//! `assets/` subdirectories are not parsed by the loader — the agent
//! discovers and reads them on demand via existing tools, with
//! `skill_root` exposed alongside the body.
//!
//! The loader also accepts the *collection* layout the `npx skills` CLI
//! produces, where a single package (e.g. `superpowers`) installs a
//! whole bundle of related skills under a shared parent directory:
//! `<root>/<collection>/<skill>/SKILL.md`. When an immediate subdir of
//! a search root has no `SKILL.md` of its own, the loader peeks one
//! level deeper for skill folders. Two levels is the cap — going
//! further would over-match arbitrary repos.
//!
//! Three search roots merge into one registry, in this order:
//!
//! 1. The configured `skills_dir` (defaults to `~/.vulcan/skills`).
//! 2. The XDG-compliant location `~/.config/vulcan/skills`.
//! 3. The bundled fallback `vulcan/skills/`.
//!
//! Later roots shadow earlier ones by skill `name` so a user copy in
//! `~/.config` overrides a bundled default.
//!
//! Discovery is metadata-only — bodies load lazily through
//! [`Skill::load_body`] when `SkillsHook` activates a skill.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SKILL_FILENAME: &str = "SKILL.md";

/// One discovered skill. Body is *not* loaded at discovery — call
/// [`Skill::load_body`] when the skill is activated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Spec-optional metadata.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Directory containing this skill's `SKILL.md` (and any optional
    /// `scripts/`, `references/`, `assets/` subdirs). Exposed so the
    /// agent can load referenced files via `read_file` / `bash`.
    pub skill_root: PathBuf,
}

impl Skill {
    /// Read the body of `<skill_root>/SKILL.md` with the YAML
    /// frontmatter removed. Lazy — the registry never calls this at
    /// discovery time.
    pub fn load_body(&self) -> Result<String> {
        let path = self.skill_root.join(SKILL_FILENAME);
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(strip_frontmatter(&raw))
    }
}

/// Discovered set of skills across configured search roots.
pub struct SkillRegistry {
    dirs: Vec<PathBuf>,
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Build a registry from an explicit list of search roots. Order
    /// matters: a skill name appearing in a later root replaces the
    /// same name from an earlier root.
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        let mut registry = Self {
            dirs,
            skills: Vec::new(),
        };
        if let Err(e) = registry.discover() {
            tracing::warn!("skill discovery failed: {e}");
        }
        registry
    }

    /// Build a registry covering the standard search locations.
    ///
    /// Search order (earliest is shadowed by later entries on name
    /// collision, so the most-specific source wins):
    ///
    /// 1. The bundled directory (`vulcan/skills/`).
    /// 2. The XDG location (`~/.config/vulcan/skills`).
    /// 3. The configured user `primary` (defaults to `~/.vulcan/skills`).
    /// 4. `~/.agents/skills` — install location for the open `npx skills`
    ///    CLI shared across the agentskills.io ecosystem.
    /// 5. Project-root `<project>/.vulcan/skills` (when supplied).
    /// 6. Project-root `<project>/.agents/skills` (when supplied).
    ///
    /// `project_root` is typically the agent's working directory. Pass
    /// `None` for callers that don't have a workspace concept.
    pub fn default_for(primary: &Path, project_root: Option<&Path>) -> Self {
        Self::with_dirs(resolve_default_dirs(
            primary,
            project_root,
            std::env::var("HOME").ok().as_deref(),
            std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        ))
    }

    /// Empty registry — used in tests so they don't pull in bundled or
    /// user-state skills implicitly.
    pub fn empty() -> Self {
        Self {
            dirs: Vec::new(),
            skills: Vec::new(),
        }
    }

    fn discover(&mut self) -> Result<()> {
        let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
        for root in &self.dirs {
            if !root.is_dir() {
                continue;
            }
            let entries = match std::fs::read_dir(root) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("read_dir {}: {e}", root.display());
                    continue;
                }
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                if dir.join(SKILL_FILENAME).is_file() {
                    Self::ingest(&dir, &mut by_name);
                    continue;
                }
                // Collection layout: `<root>/<collection>/<skill>/SKILL.md`.
                // The `npx skills` CLI installs packages this way (e.g.
                // `~/.agents/skills/superpowers/<each-skill>/SKILL.md`), so
                // peek one level deeper before giving up.
                let inner_entries = match std::fs::read_dir(&dir) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for inner in inner_entries.flatten() {
                    let inner_dir = inner.path();
                    if inner_dir.is_dir() && inner_dir.join(SKILL_FILENAME).is_file() {
                        Self::ingest(&inner_dir, &mut by_name);
                    }
                }
            }
        }
        self.skills = by_name.into_values().collect();
        Ok(())
    }

    fn ingest(skill_root: &Path, by_name: &mut BTreeMap<String, Skill>) {
        match Self::load_metadata(skill_root) {
            Ok(skill) => {
                by_name.insert(skill.name.clone(), skill);
            }
            Err(e) => {
                tracing::warn!("skip skill at {}: {e}", skill_root.display());
            }
        }
    }

    fn load_metadata(skill_root: &Path) -> Result<Skill> {
        let path = skill_root.join(SKILL_FILENAME);
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let fm = parse_frontmatter(&raw)
            .ok_or_else(|| anyhow!("missing YAML frontmatter in {}", path.display()))?;
        let name = fm
            .get_str("name")
            .ok_or_else(|| anyhow!("frontmatter missing `name` in {}", path.display()))?
            .to_string();
        let description = fm
            .get_str("description")
            .ok_or_else(|| anyhow!("frontmatter missing `description` in {}", path.display()))?
            .to_string();
        Ok(Skill {
            name,
            description,
            version: fm.get_str("version").map(str::to_string),
            license: fm.get_str("license").map(str::to_string),
            allowed_tools: fm.get_list("allowed-tools").unwrap_or_default(),
            skill_root: skill_root.to_path_buf(),
        })
    }

    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// All configured search roots, in order.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Primary path for writing new skill drafts. Falls back to
    /// `~/.vulcan/skills` when the registry was built with no dirs.
    pub fn skills_dir(&self) -> PathBuf {
        self.dirs
            .first()
            .cloned()
            .unwrap_or_else(|| crate::config::vulcan_home().join("skills"))
    }

    /// YYC-225: walk SKILL.md files for declared `extension:` blocks.
    /// Skills without an `extension:` block are silently ignored.
    pub fn drafts(&self) -> Vec<crate::extensions::ExtensionMetadata> {
        let mut out = Vec::new();
        for skill in &self.skills {
            let path = skill.skill_root.join(SKILL_FILENAME);
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(meta) = crate::extensions::parse_skill_extension(&content) {
                out.push(meta);
            }
        }
        out
    }
}

/// Pure resolver for the standard search-root list. Takes env values
/// as arguments so tests don't need to mutate the process environment.
fn resolve_default_dirs(
    primary: &Path,
    project_root: Option<&Path>,
    home: Option<&str>,
    xdg_config_home: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let push_unique = |dirs: &mut Vec<PathBuf>, path: PathBuf| {
        if dirs.iter().all(|p| p != &path) {
            dirs.push(path);
        }
    };
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
    if bundled.is_dir() {
        push_unique(&mut dirs, bundled);
    }
    if let Some(xdg) = xdg_skills_dir(home, xdg_config_home) {
        push_unique(&mut dirs, xdg);
    }
    push_unique(&mut dirs, primary.to_path_buf());
    if let Some(home_agents) = home_agents_skills_dir(home) {
        push_unique(&mut dirs, home_agents);
    }
    if let Some(root) = project_root {
        push_unique(&mut dirs, root.join(".vulcan").join("skills"));
        push_unique(&mut dirs, root.join(".agents").join("skills"));
    }
    dirs
}

/// Resolve `~/.agents/skills` — install location for the open
/// `npx skills` CLI. Shared with other clients in the agentskills.io
/// ecosystem so a skill installed for one tool is visible to Vulcan
/// automatically.
fn home_agents_skills_dir(home: Option<&str>) -> Option<PathBuf> {
    let home = home?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".agents").join("skills"))
}

/// Resolve `~/.config/vulcan/skills` (honoring `XDG_CONFIG_HOME`).
fn xdg_skills_dir(home: Option<&str>, xdg_config_home: Option<&str>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("vulcan").join("skills"));
        }
    }
    let home = home?;
    if home.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("vulcan")
            .join("skills"),
    )
}

#[derive(Debug, Default)]
struct Frontmatter {
    map: BTreeMap<String, FrontmatterValue>,
}

#[derive(Debug, Clone)]
enum FrontmatterValue {
    String(String),
    List(Vec<String>),
}

impl Frontmatter {
    fn get_str(&self, key: &str) -> Option<&str> {
        match self.map.get(key)? {
            FrontmatterValue::String(s) => Some(s.as_str()),
            FrontmatterValue::List(_) => None,
        }
    }
    fn get_list(&self, key: &str) -> Option<Vec<String>> {
        match self.map.get(key)? {
            FrontmatterValue::List(v) => Some(v.clone()),
            FrontmatterValue::String(_) => None,
        }
    }
}

/// Minimal flat-YAML parser sufficient for the SKILL.md frontmatter
/// keys defined by the spec. Strings (optionally quoted) and bracketed
/// lists are supported. Nested blocks are not — the agent-skills spec
/// doesn't require them.
fn parse_frontmatter(raw: &str) -> Option<Frontmatter> {
    let raw = raw.trim_start_matches('\u{feff}').trim_start();
    let after = raw.strip_prefix("---")?;
    let end = after.find("\n---")?;
    let body = &after[..end];
    let mut fm = Frontmatter::default();
    for line in body.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // Skip indented sub-blocks (e.g. `extension:` declarations parsed
        // separately by `extensions::parse_skill_extension`).
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let (key, value) = line.split_once(':')?;
        let key = key.trim().to_string();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if value.starts_with('[') && value.ends_with(']') {
            let inner = &value[1..value.len() - 1];
            let items: Vec<String> = inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            fm.map.insert(key, FrontmatterValue::List(items));
        } else {
            let unquoted = value.trim_matches('"').trim_matches('\'').to_string();
            fm.map.insert(key, FrontmatterValue::String(unquoted));
        }
    }
    Some(fm)
}

fn strip_frontmatter(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('\u{feff}').trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            // `---` closer is 4 bytes including the leading newline; skip
            // it then trim whatever leading whitespace the body has.
            let after = &rest[end + 4..];
            return after.trim_start().to_string();
        }
    }
    raw.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(SKILL_FILENAME),
            format!("---\n{frontmatter}\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn discovers_folder_layout_skill() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "debug",
            "name: debug\ndescription: Systematic debugging workflow",
            "## Phase 1\nReproduce.",
        );
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].name, "debug");
        assert_eq!(reg.list()[0].description, "Systematic debugging workflow");
        assert_eq!(reg.list()[0].skill_root, dir.path().join("debug"));
    }

    #[test]
    fn body_loads_lazily_after_metadata_discovery() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "debug",
            "name: debug\ndescription: d",
            "BODY_TEXT",
        );
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        let body = reg.list()[0].load_body().unwrap();
        assert!(body.contains("BODY_TEXT"));
        assert!(!body.contains("---"));
    }

    #[test]
    fn parses_optional_spec_fields() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "review",
            "name: review\ndescription: d\nversion: 1.2.3\nlicense: MIT\nallowed-tools: [read_file, bash]",
            "body",
        );
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        let s = &reg.list()[0];
        assert_eq!(s.version.as_deref(), Some("1.2.3"));
        assert_eq!(s.license.as_deref(), Some("MIT"));
        assert_eq!(
            s.allowed_tools,
            vec!["read_file".to_string(), "bash".to_string()]
        );
    }

    #[test]
    fn missing_frontmatter_is_skipped_with_warning() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("broken");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join(SKILL_FILENAME), "# just a markdown body").unwrap();
        // also a valid one so the registry doesn't trivially pass on emptiness
        write_skill(dir.path(), "ok", "name: ok\ndescription: d", "body");
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        let names: Vec<_> = reg.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["ok"]);
    }

    #[test]
    fn directory_without_skill_md_is_ignored() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("empty-dir")).unwrap();
        write_skill(dir.path(), "ok", "name: ok\ndescription: d", "body");
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].name, "ok");
    }

    #[test]
    fn later_dir_shadows_earlier_dir_by_name() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_skill(
            bundled.path(),
            "debug",
            "name: debug\ndescription: bundled-default",
            "BUNDLED_BODY",
        );
        write_skill(
            user.path(),
            "debug",
            "name: debug\ndescription: user-override",
            "USER_BODY",
        );
        // bundled goes first; user's later entry should win.
        let reg = SkillRegistry::with_dirs(vec![
            bundled.path().to_path_buf(),
            user.path().to_path_buf(),
        ]);
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].description, "user-override");
        let body = reg.list()[0].load_body().unwrap();
        assert!(body.contains("USER_BODY"));
    }

    #[test]
    fn empty_registry_has_no_skills() {
        let reg = SkillRegistry::empty();
        assert!(reg.is_empty());
        assert_eq!(reg.list().len(), 0);
    }

    #[test]
    fn nonexistent_dir_is_silently_skipped() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope");
        let reg = SkillRegistry::with_dirs(vec![missing]);
        assert!(reg.is_empty());
    }

    #[test]
    fn default_for_includes_project_root_paths() {
        let primary = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_skill(
            primary.path(),
            "user",
            "name: user\ndescription: user-global",
            "USER",
        );
        let project_vulcan = project.path().join(".vulcan").join("skills");
        std::fs::create_dir_all(&project_vulcan).unwrap();
        write_skill(
            &project_vulcan,
            "proj-vulcan",
            "name: proj-vulcan\ndescription: project .vulcan",
            "P_VULCAN",
        );
        let project_agents = project.path().join(".agents").join("skills");
        std::fs::create_dir_all(&project_agents).unwrap();
        write_skill(
            &project_agents,
            "proj-agents",
            "name: proj-agents\ndescription: project .agents",
            "P_AGENTS",
        );
        let reg = SkillRegistry::default_for(primary.path(), Some(project.path()));
        let names: Vec<_> = reg.list().iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"user".to_string()));
        assert!(names.contains(&"proj-vulcan".to_string()));
        assert!(names.contains(&"proj-agents".to_string()));
    }

    #[test]
    fn project_agents_skill_overrides_user_skill_with_same_name() {
        let primary = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_skill(
            primary.path(),
            "debug",
            "name: debug\ndescription: user-version",
            "USER_BODY",
        );
        let project_agents = project.path().join(".agents").join("skills");
        std::fs::create_dir_all(&project_agents).unwrap();
        write_skill(
            &project_agents,
            "debug",
            "name: debug\ndescription: project-version",
            "PROJECT_BODY",
        );
        let reg = SkillRegistry::default_for(primary.path(), Some(project.path()));
        let debug = reg.list().iter().find(|s| s.name == "debug").unwrap();
        assert_eq!(debug.description, "project-version");
        assert!(debug.load_body().unwrap().contains("PROJECT_BODY"));
    }

    #[test]
    fn resolve_default_dirs_includes_home_agents_skills_dir() {
        let home = tempdir().unwrap();
        let primary = tempdir().unwrap();
        let dirs = resolve_default_dirs(primary.path(), None, home.path().to_str(), None);
        let expected = home.path().join(".agents").join("skills");
        assert!(
            dirs.contains(&expected),
            "resolve_default_dirs should include {} (got {dirs:?})",
            expected.display()
        );
    }

    #[test]
    fn resolve_default_dirs_honors_xdg_config_home_override() {
        let home = tempdir().unwrap();
        let xdg = tempdir().unwrap();
        let primary = tempdir().unwrap();
        let dirs = resolve_default_dirs(
            primary.path(),
            None,
            home.path().to_str(),
            xdg.path().to_str(),
        );
        // XDG-derived path lives under the override, not under HOME/.config.
        let xdg_skills = xdg.path().join("vulcan").join("skills");
        assert!(dirs.contains(&xdg_skills));
        let default_xdg = home.path().join(".config").join("vulcan").join("skills");
        assert!(!dirs.contains(&default_xdg));
    }

    #[test]
    fn resolve_default_dirs_includes_project_paths_when_supplied() {
        let primary = tempdir().unwrap();
        let project = tempdir().unwrap();
        let dirs = resolve_default_dirs(primary.path(), Some(project.path()), None, None);
        assert!(dirs.contains(&project.path().join(".vulcan").join("skills")));
        assert!(dirs.contains(&project.path().join(".agents").join("skills")));
    }

    #[test]
    fn discovers_skills_in_collection_layout() {
        // Mirrors the `npx skills` superpowers layout:
        //   <root>/superpowers/brainstorming/SKILL.md
        //   <root>/superpowers/executing-plans/SKILL.md
        let dir = tempdir().unwrap();
        let collection = dir.path().join("superpowers");
        std::fs::create_dir_all(&collection).unwrap();
        write_skill(
            &collection,
            "brainstorming",
            "name: brainstorming\ndescription: explores ideas before implementation",
            "BS",
        );
        write_skill(
            &collection,
            "executing-plans",
            "name: executing-plans\ndescription: walks an implementation plan to completion",
            "EX",
        );
        // Sibling top-level skill should still be picked up.
        write_skill(
            dir.path(),
            "find-skills",
            "name: find-skills\ndescription: helps find new skills to install",
            "FS",
        );
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        let mut names: Vec<_> = reg.list().iter().map(|s| s.name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "brainstorming".to_string(),
                "executing-plans".to_string(),
                "find-skills".to_string(),
            ]
        );
        let bs = reg
            .list()
            .iter()
            .find(|s| s.name == "brainstorming")
            .unwrap();
        assert_eq!(bs.skill_root, collection.join("brainstorming"));
    }

    #[test]
    fn collection_recursion_is_capped_at_one_level() {
        // <root>/level1/level2/level3/SKILL.md should NOT be found —
        // only one extra level of nesting is allowed beyond the root.
        let dir = tempdir().unwrap();
        let level2 = dir.path().join("level1").join("level2");
        std::fs::create_dir_all(&level2).unwrap();
        write_skill(
            &level2,
            "deep",
            "name: deep\ndescription: too deep to discover",
            "D",
        );
        let reg = SkillRegistry::with_dirs(vec![dir.path().to_path_buf()]);
        assert!(
            reg.list().iter().all(|s| s.name != "deep"),
            "skill nested 3 levels deep should not be discovered"
        );
    }

    #[test]
    fn parse_frontmatter_handles_quoted_values() {
        let raw = "---\nname: \"quoted\"\ndescription: 'single'\n---\nbody";
        let fm = parse_frontmatter(raw).unwrap();
        assert_eq!(fm.get_str("name"), Some("quoted"));
        assert_eq!(fm.get_str("description"), Some("single"));
    }

    #[test]
    fn strip_frontmatter_returns_body_only() {
        let raw = "---\nname: x\ndescription: y\n---\n\n## Body\nhello";
        let body = strip_frontmatter(raw);
        assert_eq!(body, "## Body\nhello");
    }
}
