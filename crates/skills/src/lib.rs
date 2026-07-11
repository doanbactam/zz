//! Skills system for ZeroZero — load SKILL.md / command markdown from
//! directories and expose them as slash-invocable skills (Grok-style registry).
//!
//! Discovery mirrors the multi-source slash model used by Grok CLI:
//! - **Built-in** slash commands live in the TUI (`crates/tui::slash`) and always win.
//! - **Skills** (`SKILL.md` packages + flat `*.md`) load from project/user dirs.
//! - **Legacy commands** are flat `*.md` under any `commands/` dir (Claude/Grok layout).
//! - Higher-priority directories override lower ones by skill name (first wins).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Where a skill was loaded from (for `/project:name` vs `/user:name` / `/local:name`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    Project,
    User,
}

/// Best-effort home directory (`HOME`, then Windows `USERPROFILE`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// A skill/command directory with its slash-command scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDir {
    pub path: PathBuf,
    pub scope: SkillScope,
}

/// Standard skill/command directories in **priority order** (high → low).
///
/// Project (cwd), then user home. Within each tier:
/// 1. `.zerozero/{skills,commands}` — primary ZeroZero layout
/// 2. `.agents/{skills,commands}` — shared agent layout
/// 3. `.claude/{skills,commands}` — Claude Code / Grok harness compat
/// 4. `.grok/{skills,commands}` — Grok CLI project pack
/// 5. `.devin/skills` — legacy ZeroZero path (still loaded)
///
/// User tier also loads `~/.config/zerozero/{skills,commands}` and
/// `~/.config/devin/skills` (legacy), plus `~/{.agents,.claude,.grok}/…`.
///
/// Paths are returned even if they do not exist yet (callers may pass them to
/// the TUI for hot-reload). Use [`SkillRegistry::load_from_skill_dirs`].
pub fn standard_skill_dirs(cwd: &Path, home: Option<&Path>) -> Vec<SkillDir> {
    let mut dirs = Vec::new();

    fn push_pair(dirs: &mut Vec<SkillDir>, root: &Path, name: &str, scope: SkillScope) {
        dirs.push(SkillDir {
            path: root.join(name).join("skills"),
            scope,
        });
        dirs.push(SkillDir {
            path: root.join(name).join("commands"),
            scope,
        });
    }

    // Project-level (highest priority first).
    for name in [".zerozero", ".agents", ".claude", ".grok"] {
        push_pair(&mut dirs, cwd, name, SkillScope::Project);
    }
    dirs.push(SkillDir {
        path: cwd.join(".devin").join("skills"),
        scope: SkillScope::Project,
    });

    // User-level (lowest priority).
    if let Some(home) = home {
        dirs.push(SkillDir {
            path: home.join(".config").join("zerozero").join("skills"),
            scope: SkillScope::User,
        });
        dirs.push(SkillDir {
            path: home.join(".config").join("zerozero").join("commands"),
            scope: SkillScope::User,
        });
        dirs.push(SkillDir {
            path: home.join(".config").join("devin").join("skills"),
            scope: SkillScope::User,
        });
        for name in [".agents", ".claude", ".grok"] {
            push_pair(&mut dirs, home, name, SkillScope::User);
        }
    }

    dirs
}

/// Paths only (order preserved) — for TUI `skill_dirs` / reload lists.
pub fn standard_skill_paths(cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    standard_skill_dirs(cwd, home)
        .into_iter()
        .map(|d| d.path)
        .collect()
}

/// Convenience: [`standard_skill_dirs`] using `current_dir` + [`home_dir`].
pub fn discover_skill_dirs() -> Vec<SkillDir> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    standard_skill_dirs(&cwd, home_dir().as_deref())
}

/// Convenience paths for callers that only need `Vec<PathBuf>`.
pub fn discover_skill_paths() -> Vec<PathBuf> {
    discover_skill_dirs().into_iter().map(|d| d.path).collect()
}

/// A skill loaded from a SKILL.md file.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub content: String,
    pub source_path: PathBuf,
    pub scope: SkillScope,
    /// When false, skill is omitted from slash menu and `/name` invocation.
    pub user_invocable: bool,
    pub argument_hint: String,
}

/// Metadata for slash autocomplete (skills only; built-ins live in `tui::slash`).
#[derive(Debug, Clone)]
pub struct SkillSlashEntry {
    pub name: String,
    pub scope: SkillScope,
    pub description: String,
    pub argument_hint: String,
}

/// Registry of loaded skills. Searches directories for SKILL.md files.
pub struct SkillRegistry {
    skills: Vec<Skill>,
    dirs: Vec<SkillDir>,
}

impl SkillRegistry {
    /// Create empty registry.
    pub const fn new() -> Self {
        Self {
            skills: Vec::new(),
            dirs: Vec::new(),
        }
    }

    /// Reload all skills from the stored directories.
    /// Clears existing skills and re-reads from disk.
    pub fn reload(&mut self) -> anyhow::Result<usize> {
        self.skills.clear();
        let dirs = self.dirs.clone();
        // Clear recorded dirs so load re-registers them.
        self.dirs.clear();
        self.load_from_skill_dirs(&dirs)
    }

    /// Load skills from a directory with inferred scope (path heuristic).
    /// Prefer [`Self::load_from_dir_scoped`] when scope is known.
    pub fn load_from_dir(&mut self, dir: &Path) -> anyhow::Result<usize> {
        self.load_from_dir_scoped(dir, infer_scope(dir))
    }

    /// Load skills from a directory. Looks for `<dir>/<name>/SKILL.md`
    /// or `<dir>/<name>.md` files. Each SKILL.md has optional YAML
    /// frontmatter (between --- lines) with name, description, keywords.
    /// The rest is markdown content.
    ///
    /// If `dir` doesn't exist, returns `Ok(0)` (not an error).
    /// Name collisions are skipped (first-loaded wins).
    pub fn load_from_dir_scoped(&mut self, dir: &Path, scope: SkillScope) -> anyhow::Result<usize> {
        if !self.dirs.iter().any(|d| d.path == dir) {
            self.dirs.push(SkillDir {
                path: dir.to_path_buf(),
                scope,
            });
        }
        if !dir.exists() {
            return Ok(0);
        }

        let mut count = 0;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                // Look for `<dir>/<name>/SKILL.md`
                let skill_md = path.join("SKILL.md");
                if skill_md.is_file() {
                    if let Some(skill) = load_skill_file(&skill_md, scope)? {
                        if self.push_skill(skill) {
                            count += 1;
                        }
                    }
                }
            } else if file_type.is_file() {
                // Flat `*.md` — including Claude/Grok-style `commands/<name>.md`.
                if let Some(ext) = path.extension() {
                    if ext == "md" {
                        if let Some(skill) = load_skill_file(&path, scope)? {
                            if self.push_skill(skill) {
                                count += 1;
                            }
                        }
                    }
                }
            }
        }

        Ok(count)
    }

    /// Load from multiple path-only directories (scope inferred per path).
    pub fn load_from_dirs(&mut self, dirs: &[PathBuf]) -> anyhow::Result<usize> {
        let mut total = 0;
        for dir in dirs {
            total += self.load_from_dir(dir)?;
        }
        Ok(total)
    }

    /// Load from [`standard_skill_dirs`] entries (explicit scope, priority order).
    pub fn load_from_skill_dirs(&mut self, dirs: &[SkillDir]) -> anyhow::Result<usize> {
        let mut total = 0;
        for d in dirs {
            total += self.load_from_dir_scoped(&d.path, d.scope)?;
        }
        Ok(total)
    }

    /// Discover + load standard skill/command dirs for cwd + home.
    pub fn load_standard(&mut self) -> anyhow::Result<usize> {
        let dirs = discover_skill_dirs();
        self.load_from_skill_dirs(&dirs)
    }

    /// Insert skill if no skill with the same name (case-insensitive) exists yet.
    /// Returns `true` when inserted.
    fn push_skill(&mut self, skill: Skill) -> bool {
        if self
            .skills
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&skill.name))
        {
            return false;
        }
        self.skills.push(skill);
        true
    }

    /// Get the directories this registry loads from (with scope).
    pub fn skill_dirs(&self) -> &[SkillDir] {
        &self.dirs
    }

    /// Paths only (compat helper).
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.dirs.iter().map(|d| d.path.clone()).collect()
    }

    /// Find a skill by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Skills the user can invoke via `/name` or `/project:name` / `/user:name`.
    pub fn slash_entries(&self) -> Vec<SkillSlashEntry> {
        self.skills
            .iter()
            .filter(|s| s.user_invocable)
            .map(|s| SkillSlashEntry {
                name: s.name.clone(),
                scope: s.scope,
                description: s.description.clone(),
                argument_hint: s.argument_hint.clone(),
            })
            .collect()
    }

    /// Resolve a slash token (`ponytail`, `project:ponytail`, `local:ponytail`, `user:ponytail`).
    pub fn resolve_slash_token(&self, token: &str) -> Option<&Skill> {
        let (scope, name) = parse_slash_token(token)?;
        let matches: Vec<&Skill> = self
            .skills
            .iter()
            .filter(|s| s.user_invocable && s.name.eq_ignore_ascii_case(&name))
            .filter(|s| scope.is_none_or(|sc| s.scope == sc))
            .collect();
        if matches.is_empty() {
            return None;
        }
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        // Ambiguous: prefer project over user.
        matches
            .iter()
            .find(|s| s.scope == SkillScope::Project)
            .copied()
            .or(Some(matches[0]))
    }

    /// Search skills by keyword match.
    pub fn search(&self, query: &str) -> Vec<&Skill> {
        let query_lower = query.to_lowercase();
        self.skills
            .iter()
            .filter(|s| {
                s.keywords
                    .iter()
                    .any(|k| k.to_lowercase().contains(&query_lower))
                    || s.name.to_lowercase().contains(&query_lower)
                    || s.description.to_lowercase().contains(&query_lower)
            })
            .collect()
    }

    /// List all skill names.
    pub fn list(&self) -> Vec<String> {
        self.skills.iter().map(|s| s.name.clone()).collect()
    }

    /// Get all skills.
    pub fn all(&self) -> &[Skill] {
        &self.skills
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Fallback scope inference when callers only pass a bare path.
/// Prefer explicit [`SkillDir::scope`] via [`SkillRegistry::load_from_skill_dirs`].
fn infer_scope(dir: &Path) -> SkillScope {
    let s = dir.to_string_lossy().replace('\\', "/");
    if s.contains("/.config/") {
        return SkillScope::User;
    }
    for marker in [
        "/.zerozero/skills",
        "/.zerozero/commands",
        "/.agents/skills",
        "/.agents/commands",
        "/.claude/skills",
        "/.claude/commands",
        "/.grok/skills",
        "/.grok/commands",
        "/.devin/skills",
    ] {
        if s.contains(marker) {
            return SkillScope::Project;
        }
    }
    SkillScope::User
}

/// Parse `project:foo`, `local:foo`, or `user:foo`.
pub fn parse_slash_token(token: &str) -> Option<(Option<SkillScope>, String)> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if let Some((pfx, name)) = token.split_once(':') {
        if !name.is_empty() {
            let scope = match pfx.to_ascii_lowercase().as_str() {
                "project" | "local" => Some(SkillScope::Project),
                "user" => Some(SkillScope::User),
                _ => None,
            };
            if scope.is_some() {
                return Some((scope, name.to_string()));
            }
        }
    }
    Some((None, token.to_string()))
}

fn load_skill_file(path: &Path, scope: SkillScope) -> anyhow::Result<Option<Skill>> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let (meta, body) = parse_frontmatter(&raw);

    let name = meta
        .get("name")
        .cloned()
        .or_else(|| {
            // Derive name from parent dir (for `<dir>/<name>/SKILL.md`) or file stem.
            if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            } else {
                path.file_stem()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            }
        })
        .unwrap_or_else(|| "unnamed".to_string());

    let description = meta.get("description").cloned().unwrap_or_default();

    let keywords = meta
        .get("keywords")
        .map(|s| {
            s.split(',')
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let user_invocable = match meta
        .get("user-invocable")
        .or_else(|| meta.get("user_invocable"))
    {
        Some(v) if v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("no") => false,
        Some(v) if v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes") => true,
        _ => true,
    };
    let argument_hint = meta
        .get("argument-hint")
        .or_else(|| meta.get("argument_hint"))
        .cloned()
        .unwrap_or_default();

    Ok(Some(Skill {
        name,
        description,
        keywords,
        content: body,
        source_path: path.to_path_buf(),
        scope,
        user_invocable,
        argument_hint,
    }))
}

/// Parse YAML-like frontmatter from markdown content.
/// Returns (frontmatter_fields, body_content).
/// Frontmatter is between `---` delimiters at the start of the file.
fn parse_frontmatter(content: &str) -> (HashMap<String, String>, String) {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    // Must start with "---\n" (or "---\r\n").
    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"));

    let Some(after_open) = after_open else {
        return (HashMap::new(), content.to_string());
    };

    // Find the closing "---" delimiter on its own line.
    let mut fields = HashMap::new();
    let mut rest = after_open;
    let mut found_close = false;

    for line in after_open.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "---" {
            found_close = true;
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if !key.is_empty() {
                fields.insert(key, value);
            }
        }
        rest = &rest[line.len()..];
        // Consume the newline that follows.
        if let Some(stripped) = rest.strip_prefix('\n') {
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix("\r\n") {
            rest = stripped;
        }
    }

    if !found_close {
        // No closing delimiter — treat whole content as body, no frontmatter.
        return (HashMap::new(), content.to_string());
    }

    let body = rest.to_string();
    (fields, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_frontmatter_with_metadata() {
        let content = "---\nname: my-skill\ndescription: A test skill\nkeywords: rust, testing\n---\n# My Skill\nThis is the body.";
        let (meta, body) = parse_frontmatter(content);
        assert_eq!(meta.get("name").unwrap(), "my-skill");
        assert_eq!(meta.get("description").unwrap(), "A test skill");
        assert_eq!(meta.get("keywords").unwrap(), "rust, testing");
        assert!(body.contains("# My Skill"));
        assert!(body.contains("This is the body."));
        assert!(!body.contains("name: my-skill"));
    }

    #[test]
    fn test_parse_frontmatter_without_metadata() {
        let content = "# Just markdown\nNo frontmatter.";
        let (meta, body) = parse_frontmatter(content);
        assert!(meta.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn test_parse_frontmatter_no_closing_delimiter() {
        let content = "---\nname: my-skill\nno closing delimiter here";
        let (meta, body) = parse_frontmatter(content);
        assert!(meta.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn test_parse_slash_token() {
        assert_eq!(
            parse_slash_token("project:foo"),
            Some((Some(SkillScope::Project), "foo".to_string()))
        );
        assert_eq!(
            parse_slash_token("local:foo"),
            Some((Some(SkillScope::Project), "foo".to_string()))
        );
        assert_eq!(
            parse_slash_token("user:bar"),
            Some((Some(SkillScope::User), "bar".to_string()))
        );
        assert_eq!(
            parse_slash_token("ponytail"),
            Some((None, "ponytail".to_string()))
        );
    }

    #[test]
    fn test_user_invocable_false() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("hidden")).unwrap();
        std::fs::write(
            skills_dir.join("hidden/SKILL.md"),
            "---\nname: hidden\nuser-invocable: false\n---\nbody",
        )
        .unwrap();
        let mut reg = SkillRegistry::new();
        reg.load_from_dir(&skills_dir).unwrap();
        assert!(reg.slash_entries().is_empty());
        assert!(reg.resolve_slash_token("hidden").is_none());
    }

    #[test]
    fn test_skill_registry_load_from_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(skills_dir.join("my-skill")).unwrap();
        let mut f = fs::File::create(skills_dir.join("my-skill").join("SKILL.md")).unwrap();
        writeln!(
            f,
            "---\nname: my-skill\ndescription: Test\nkeywords: test\n---\n# My Skill\nBody"
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        let count = reg.load_from_dir(&skills_dir).unwrap();
        assert_eq!(count, 1);
        assert_eq!(reg.get("my-skill").unwrap().description, "Test");
        assert_eq!(reg.get("my-skill").unwrap().keywords, vec!["test"]);
        assert!(reg.get("my-skill").unwrap().content.contains("# My Skill"));
    }

    #[test]
    fn test_skill_registry_load_from_md_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        let mut f = fs::File::create(skills_dir.join("rust-tips.md")).unwrap();
        writeln!(
            f,
            "---\nname: rust-tips\ndescription: Rust tips\nkeywords: rust, idioms\n---\nUse clippy."
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        let count = reg.load_from_dir(&skills_dir).unwrap();
        assert_eq!(count, 1);
        assert_eq!(reg.get("rust-tips").unwrap().description, "Rust tips");
    }

    #[test]
    fn test_skill_registry_load_name_from_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(skills_dir.join("git-flow")).unwrap();
        let mut f = fs::File::create(skills_dir.join("git-flow").join("SKILL.md")).unwrap();
        writeln!(f, "# Git Flow\nNo frontmatter here.").unwrap();

        let mut reg = SkillRegistry::new();
        let count = reg.load_from_dir(&skills_dir).unwrap();
        assert_eq!(count, 1);
        assert_eq!(reg.get("git-flow").unwrap().name, "git-flow");
    }

    #[test]
    fn test_skill_registry_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");

        fs::create_dir_all(skills_dir.join("rust")).unwrap();
        let mut f = fs::File::create(skills_dir.join("rust").join("SKILL.md")).unwrap();
        writeln!(
            f,
            "---\nname: rust\ndescription: Rust idioms\nkeywords: rust, cargo, clippy\n---\nbody"
        )
        .unwrap();

        fs::create_dir_all(skills_dir.join("python")).unwrap();
        let mut f = fs::File::create(skills_dir.join("python").join("SKILL.md")).unwrap();
        writeln!(
            f,
            "---\nname: python\ndescription: Python tips\nkeywords: python, pip\n---\nbody"
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dir(&skills_dir).unwrap();

        let rust_results = reg.search("rust");
        assert_eq!(rust_results.len(), 1);
        assert_eq!(rust_results[0].name, "rust");

        let cargo_results = reg.search("cargo");
        assert_eq!(cargo_results.len(), 1);
        assert_eq!(cargo_results[0].name, "rust");

        let py_results = reg.search("python");
        assert_eq!(py_results.len(), 1);
        assert_eq!(py_results[0].name, "python");

        let none = reg.search("java");
        assert!(none.is_empty());
    }

    #[test]
    fn test_skill_registry_nonexistent_dir() {
        let mut reg = SkillRegistry::new();
        let count = reg
            .load_from_dir(Path::new("/nonexistent/path/that/does/not/exist"))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_skill_registry_load_from_dirs() {
        let tmp1 = tempfile::TempDir::new().unwrap();
        let dir1 = tmp1.path().join("skills1");
        fs::create_dir_all(dir1.join("a")).unwrap();
        let mut f = fs::File::create(dir1.join("a").join("SKILL.md")).unwrap();
        writeln!(f, "---\nname: a\ndescription: A\n---\nbody").unwrap();

        let tmp2 = tempfile::TempDir::new().unwrap();
        let dir2 = tmp2.path().join("skills2");
        fs::create_dir_all(dir2.join("b")).unwrap();
        let mut f = fs::File::create(dir2.join("b").join("SKILL.md")).unwrap();
        writeln!(f, "---\nname: b\ndescription: B\n---\nbody").unwrap();

        let mut reg = SkillRegistry::new();
        let count = reg.load_from_dirs(&[dir1, dir2]).unwrap();
        assert_eq!(count, 2);
        assert!(reg.get("a").is_some());
        assert!(reg.get("b").is_some());
    }

    #[test]
    fn test_standard_skill_dirs_priority_and_scope() {
        let cwd = PathBuf::from("repo");
        let home = PathBuf::from("home_u");
        let dirs = standard_skill_dirs(&cwd, Some(&home));
        assert_eq!(dirs[0].path, cwd.join(".zerozero").join("skills"));
        assert_eq!(dirs[0].scope, SkillScope::Project);
        assert_eq!(dirs[1].path, cwd.join(".zerozero").join("commands"));
        assert!(
            dirs.iter()
                .any(|d| d.path == cwd.join(".devin").join("skills")
                    && d.scope == SkillScope::Project)
        );
        assert!(dirs.iter().any(|d| {
            d.path == home.join(".config").join("zerozero").join("skills")
                && d.scope == SkillScope::User
        }));
        // Project entries come before user.
        let first_user = dirs
            .iter()
            .position(|d| d.scope == SkillScope::User)
            .unwrap();
        let last_project = dirs
            .iter()
            .rposition(|d| d.scope == SkillScope::Project)
            .unwrap();
        assert!(last_project < first_user);
    }

    #[test]
    fn test_priority_dedup_higher_wins() {
        let tmp = tempfile::TempDir::new().unwrap();
        let high = tmp.path().join(".zerozero/skills");
        let low = tmp.path().join(".devin/skills");
        fs::create_dir_all(high.join("same")).unwrap();
        fs::create_dir_all(low.join("same")).unwrap();
        fs::write(
            high.join("same/SKILL.md"),
            "---\nname: same\ndescription: high\n---\nfrom high",
        )
        .unwrap();
        fs::write(
            low.join("same/SKILL.md"),
            "---\nname: same\ndescription: low\n---\nfrom low",
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        let dirs = standard_skill_dirs(tmp.path(), None);
        reg.load_from_skill_dirs(&dirs).unwrap();
        assert_eq!(reg.get("same").unwrap().description, "high");
        assert!(reg.get("same").unwrap().content.contains("from high"));
        assert_eq!(reg.all().len(), 1);
    }

    #[test]
    fn test_commands_flat_md_as_slash_skill() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cmd_dir = tmp.path().join(".zerozero/commands");
        fs::create_dir_all(&cmd_dir).unwrap();
        fs::write(
            cmd_dir.join("ship.md"),
            "---\nname: ship\ndescription: Ship it\nargument-hint: notes\n---\nShip the release.",
        )
        .unwrap();
        let mut reg = SkillRegistry::new();
        let dirs = standard_skill_dirs(tmp.path(), None);
        reg.load_from_skill_dirs(&dirs).unwrap();
        let skill = reg.resolve_slash_token("ship").unwrap();
        assert_eq!(skill.scope, SkillScope::Project);
        assert_eq!(skill.argument_hint, "notes");
        assert!(skill.content.contains("Ship the release"));
    }

    #[test]
    fn test_skill_registry_list_and_all() {
        let mut reg = SkillRegistry::new();
        reg.skills.push(Skill {
            name: "x".to_string(),
            description: String::new(),
            keywords: vec![],
            content: String::new(),
            source_path: PathBuf::new(),
            scope: SkillScope::Project,
            user_invocable: true,
            argument_hint: String::new(),
        });
        reg.skills.push(Skill {
            name: "y".to_string(),
            description: String::new(),
            keywords: vec![],
            content: String::new(),
            source_path: PathBuf::new(),
            scope: SkillScope::User,
            user_invocable: true,
            argument_hint: String::new(),
        });

        let list = reg.list();
        assert_eq!(list, vec!["x", "y"]);
        assert_eq!(reg.all().len(), 2);
    }

    #[test]
    fn test_skill_registry_default() {
        let reg = SkillRegistry::default();
        assert!(reg.all().is_empty());
    }
}
