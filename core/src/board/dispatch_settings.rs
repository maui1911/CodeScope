//! What a project sets for the dispatches on it (ADR-0027): the skill each round runs, and the
//! setup command for a repository whose default branch has no setup script. Kept on the project
//! in `projects.json`, empty by default.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use super::dispatch::Round;

/// The skill a repair runs unless the project names another.
pub const REPAIR_SKILL: &str = "repair";

/// What a project sets for its dispatches; left empty, dispatches run as without it. A value of
/// a shape this build can't read counts as not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchSettings {
    /// Skill names that replace `research`, `implement`, `address-review` and `repair`.
    #[serde(default, skip_serializing_if = "SkillNames::is_empty", deserialize_with = "or_default")]
    pub skills: SkillNames,
    /// Runs in a new worktree whose default branch has no setup script: with PowerShell on
    /// Windows, `sh -e` elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub setup_command: Option<String>,
}

/// Skill names a project sets; `None`, or a name [`is_skill_name`] refuses, keeps the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillNames {
    /// Replaces `research`.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub research: Option<String>,
    /// Replaces `implement`.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub implement: Option<String>,
    /// Replaces `address-review`.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub address_review: Option<String>,
    /// Replaces [`REPAIR_SKILL`].
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub repair: Option<String>,
}

impl SkillNames {
    /// Whether no name is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl DispatchSettings {
    /// Whether nothing is set, so the project's row leaves the key out.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The skill `round` runs on this project.
    pub fn skill(&self, round: Round) -> &str {
        let set = match round {
            Round::Research => &self.skills.research,
            Round::Implementation => &self.skills.implement,
            Round::Review => &self.skills.address_review,
        };
        chosen(set, round.skill())
    }

    /// The skill a repair runs on this project.
    pub fn repair_skill(&self) -> &str {
        chosen(&self.skills.repair, REPAIR_SKILL)
    }
}

pub(crate) fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn chosen<'a>(set: &'a Option<String>, default: &'a str) -> &'a str {
    set.as_deref().filter(|name| is_skill_name(name)).unwrap_or(default)
}

/// Whether `name` can name a skill in the Agent Skills format: lowercase letters, digits and
/// single hyphens between them, at most 64 characters.
pub fn is_skill_name(name: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    name.len() <= 64 && name.split('-').all(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(research: &str, implement: &str, address_review: &str, repair: &str) -> SkillNames {
        let name = |name: &str| Some(name.to_string());
        SkillNames {
            research: name(research),
            implement: name(implement),
            address_review: name(address_review),
            repair: name(repair),
        }
    }

    #[test]
    fn a_project_that_sets_nothing_runs_the_default_skills() {
        let settings = DispatchSettings::default();
        assert_eq!(settings.skill(Round::Research), "research");
        assert_eq!(settings.skill(Round::Implementation), "implement");
        assert_eq!(settings.skill(Round::Review), "address-review");
        assert_eq!(settings.repair_skill(), "repair");
        assert!(settings.is_empty());
    }

    #[test]
    fn a_project_s_skill_names_replace_the_defaults() {
        let settings = DispatchSettings {
            skills: named("dig", "build-it", "answer-review", "fix-ci"),
            setup_command: None,
        };
        assert_eq!(settings.skill(Round::Research), "dig");
        assert_eq!(settings.skill(Round::Implementation), "build-it");
        assert_eq!(settings.skill(Round::Review), "answer-review");
        assert_eq!(settings.repair_skill(), "fix-ci");
        assert!(!settings.is_empty());
    }

    #[test]
    fn a_name_that_can_t_name_a_skill_keeps_the_default() {
        let settings = DispatchSettings {
            skills: named("", "../outside", "Address-Review", "fix ci"),
            setup_command: None,
        };
        assert_eq!(settings.skill(Round::Research), "research");
        assert_eq!(settings.skill(Round::Implementation), "implement");
        assert_eq!(settings.skill(Round::Review), "address-review");
        assert_eq!(settings.repair_skill(), "repair");
    }

    #[test]
    fn skill_names_follow_the_agent_skills_format() {
        for good in ["research", "fix-ci", "a1", "x".repeat(64).as_str()] {
            assert!(is_skill_name(good), "{good}");
        }
        let too_long = "x".repeat(65);
        for bad in ["", "-fix", "fix-", "fix--ci", "Fix", "fix_ci", "fix ci", "../x", "a/b"] {
            assert!(!is_skill_name(bad), "{bad}");
        }
        assert!(!is_skill_name(&too_long));
    }

    #[test]
    fn only_what_is_set_reaches_projects_json() {
        let empty = serde_json::to_string(&DispatchSettings::default()).unwrap();
        assert_eq!(empty, "{}");
        let settings = DispatchSettings {
            skills: SkillNames {
                address_review: Some("answer-review".into()),
                ..SkillNames::default()
            },
            setup_command: Some("npm ci".into()),
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(json, r#"{"skills":{"addressReview":"answer-review"},"setupCommand":"npm ci"}"#);
        assert_eq!(serde_json::from_str::<DispatchSettings>(&json).unwrap(), settings);
    }
}
