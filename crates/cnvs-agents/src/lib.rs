use std::borrow::Cow;

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "skills/"]
struct SkillAssets;

/// A skill available to agents through the `cnvs` binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
  pub slug: String,
  pub content: String,
  pub frontmatter: String,
}

/// Returns all embedded skills, ordered by slug.
#[must_use]
pub fn list_skills() -> Vec<Skill> {
  let mut skills = SkillAssets::iter()
    .filter_map(|path| skill_from_path(&path))
    .collect::<Vec<_>>();
  skills.sort_unstable_by(|left, right| left.slug.cmp(&right.slug));
  skills
}

/// Returns an embedded skill by its slug.
#[must_use]
pub fn get_skill(slug: &str) -> Option<Skill> {
  let path = format!("{slug}.md");
  SkillAssets::get(&path).and_then(|asset| skill_from_asset(slug, asset.data))
}

fn skill_from_path(path: &str) -> Option<Skill> {
  let slug = path.strip_suffix(".md")?;
  let asset = SkillAssets::get(path)?;
  skill_from_asset(slug, asset.data)
}

fn skill_from_asset(slug: &str, asset: Cow<'static, [u8]>) -> Option<Skill> {
  let content = String::from_utf8(asset.into_owned()).ok()?;
  let frontmatter = extract_frontmatter(&content).unwrap_or_default();
  Some(Skill {
    slug: slug.to_owned(),
    content,
    frontmatter,
  })
}

fn extract_frontmatter(content: &str) -> Option<String> {
  let content = content.strip_prefix("---\n")?;
  let end = content.find("\n---")?;
  Some(content.get(..end)?.to_owned())
}

#[cfg(test)]
mod tests {
  use super::{extract_frontmatter, list_skills};

  #[test]
  fn embedded_skills_have_slugs_and_frontmatter() {
    let skills = list_skills();
    assert!(!skills.is_empty());
    assert!(skills.iter().all(|skill| !skill.slug.is_empty()));
    assert!(skills.iter().all(|skill| !skill.frontmatter.is_empty()));
  }

  #[test]
  fn extracts_frontmatter_without_the_delimiters() {
    assert_eq!(
      extract_frontmatter("---\ndescription: Example\n---\nbody"),
      Some("description: Example".into())
    );
  }
}
