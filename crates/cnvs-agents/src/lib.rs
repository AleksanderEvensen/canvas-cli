use std::collections::HashMap;

use cnvs_config::Config;

mod templates;
use templates::{render_skill, render_skills};

/// A skill available to agents through the `cnvs` binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
  pub slug: String,
  pub content: String,
  pub frontmatter: HashMap<String, String>,
}

/// Returns all embedded skills, ordered by slug.
#[must_use]
pub fn list_skills(config: &Config, read_only_actions: bool) -> Vec<Skill> {
  let mut skills = render_skills(config, read_only_actions)
    .into_iter()
    .filter_map(|(slug, content)| skill_from_content(slug, &content, read_only_actions))
    .collect::<Vec<_>>();
  skills.sort_unstable_by(|left, right| left.slug.cmp(&right.slug));
  skills
}

/// Returns an embedded skill by its slug.
#[must_use]
pub fn get_skill(slug: &str, config: &Config, read_only_actions: bool) -> Option<Skill> {
  let content = render_skill(slug, config, read_only_actions)?;
  skill_from_content(slug, &content, read_only_actions)
}

fn skill_from_content(slug: &str, content: &str, read_only_actions: bool) -> Option<Skill> {
  // Injects the name into the frontmatter
  let content = format!("---\nname: {slug}\n{}", content.strip_prefix("---\n")?);

  let frontmatter = extract_frontmatter(&content)?;

  let require_write_access = frontmatter
    .get("require-write-access")
    .map_or_else(|| String::from("false"), |v| v.to_lowercase())
    == "true";

  // If we're in read-only mode and the frontmatter require-write-access then ignore this skill
  if read_only_actions && require_write_access {
    return None;
  }

  Some(Skill {
    slug: slug.to_owned(),
    content,
    frontmatter,
  })
}

fn extract_frontmatter(content: &str) -> Option<HashMap<String, String>> {
  let (frontmatter_string, _) = content.strip_prefix("---\n")?.split_once("\n---")?;

  Some(HashMap::from_iter(
    frontmatter_string
      .lines()
      .filter_map(|v| v.split_once(':'))
      .map(|(key, value)| (String::from(key.trim()), String::from(value.trim()))),
  ))
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use cnvs_config::Config;

  use super::{extract_frontmatter, list_skills};

  #[test]
  fn embedded_skills_have_slugs_and_frontmatter() {
    let skills = list_skills(&Config::default(), false);
    assert!(!skills.is_empty());
    assert!(skills.iter().all(|skill| !skill.slug.is_empty()));
    assert!(skills.iter().all(|skill| !skill.frontmatter.is_empty()));
    assert!(skills
      .iter()
      .all(|skill| skill.frontmatter.get("name") == Some(&skill.slug)));
  }

  #[test]
  fn extracts_frontmatter_without_the_delimiters() {
    assert_eq!(
      extract_frontmatter("---\ndescription: Example\n---\nbody"),
      Some(HashMap::from([(
        String::from("description"),
        String::from("Example"),
      )]))
    );
  }
}
