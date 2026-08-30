use cnvs_config::Config;
use sailfish::TemplateOnce;

pub(crate) struct SkillContext<'a> {
  pub(crate) config: &'a Config,
  pub(crate) read_only_actions: bool,
}

trait SkillTemplate {
  fn slug(&self) -> &'static str;
  fn render(self: Box<Self>) -> Option<String>;
}

macro_rules! skills {
  ($(($slug:literal, $path:literal)),+ $(,)?) => {
    fn skill_templates(
      config: &Config,
      read_only_actions: bool,
    ) -> Vec<Box<dyn SkillTemplate + '_>> {
      vec![
        $(
          {
            #[derive(TemplateOnce)]
            #[template(path = $path)]
            struct Template<'a> {
              ctx: SkillContext<'a>,
            }

            impl SkillTemplate for Template<'_> {
              fn slug(&self) -> &'static str {
                $slug
              }

              fn render(self: Box<Self>) -> Option<String> {
                self.render_once().ok()
              }
            }

            Box::new(Template {
              ctx: SkillContext {
                config,
                read_only_actions,
              },
            }) as Box<dyn SkillTemplate + '_>
          }
        ),+
      ]
    }
  };
}

skills![("cnvs-api", "cnvs-api.md"), ("cnvs", "cnvs.md")];

pub(crate) fn render_skills(
  config: &Config,
  read_only_actions: bool,
) -> Vec<(&'static str, String)> {
  skill_templates(config, read_only_actions)
    .into_iter()
    .filter_map(|template| Some((template.slug(), template.render()?)))
    .collect()
}

pub(crate) fn render_skill(slug: &str, config: &Config, read_only_actions: bool) -> Option<String> {
  render_skills(config, read_only_actions)
    .into_iter()
    .find_map(|(skill_slug, content)| (skill_slug == slug).then_some(content))
}
