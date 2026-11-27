//! What a project sets for the dispatches on it (ADR-0027): the skill each round runs, the setup
//! command for a repository whose default branch has no setup script, the agent each phase
//! runs and who reviews the pull requests. Kept on the project in `projects.json`, empty by
//! default.

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
    /// The agent each phase's session runs, in place of the project's agent.
    #[serde(
        default,
        skip_serializing_if = "PhaseAgents::is_empty",
        deserialize_with = "or_default"
    )]
    pub agents: PhaseAgents,
    /// Asked to review a pull request a dispatch opened once CodeScope moves its item to In
    /// review: GitHub logins, and teams as `org/team`.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "or_default")]
    pub reviewers: Vec<String>,
}

/// Agents a project sets per phase, by their id in the agent list; `None` or an empty id runs
/// the project's agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseAgents {
    /// Runs the research round.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub research: Option<String>,
    /// Runs the implementation round, the review rounds and the repairs.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "or_default")]
    pub implementation: Option<String>,
}

impl PhaseAgents {
    /// Whether no agent is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The people and teams a pull request asks for a review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewRequest {
    /// GitHub logins.
    pub users: Vec<String>,
    /// Teams as `org/team`; GitHub asks only those of the organisation that owns the repository.
    pub teams: Vec<String>,
}

impl ReviewRequest {
    /// Whether it asks nobody.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.teams.is_empty()
    }

    /// Whom it asks, as a sentence lists them: `ana, bob and acme/web`.
    pub fn names(&self) -> String {
        let names: Vec<&str> = self.users.iter().chain(&self.teams).map(String::as_str).collect();
        match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => names.concat(),
        }
    }
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

    /// The id of the agent that runs `round` on this project, when the project names one; a
    /// repair runs the agent of the round it repairs.
    pub fn agent(&self, round: Round) -> Option<&str> {
        let set = match round {
            Round::Research => &self.agents.research,
            Round::Implementation | Round::Review => &self.agents.implementation,
        };
        set.as_deref().filter(|id| !id.is_empty())
    }

    /// Whom a pull request by `author` asks for a review: the reviewers set, once each, without
    /// the author, whom GitHub refuses to ask, and without a name [`is_reviewer`] refuses.
    pub fn review_request(&self, author: &str) -> ReviewRequest {
        let mut request = ReviewRequest::default();
        for name in self.reviewers.iter().filter(|name| is_reviewer(name)) {
            let list = if name.contains('/') {
                &mut request.teams
            } else if name.eq_ignore_ascii_case(author) {
                continue;
            } else {
                &mut request.users
            };
            if !list.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                list.push(name.to_string());
            }
        }
        request
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

/// Whether `name` is a GitHub login, or a team as `org/team`.
pub fn is_reviewer(name: &str) -> bool {
    match name.split_once('/') {
        Some((org, team)) => is_login(org) && is_team_slug(team),
        None => is_login(name),
    }
}

fn is_login(name: &str) -> bool {
    let plain = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plain && !name.is_empty() && name.len() <= 39 && !name.starts_with('-')
}

fn is_team_slug(slug: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    !slug.is_empty() && slug.chars().all(plain)
}

/// The reviewers in `text`, separated by commas or spaces, each without a leading `@`; else the
/// first that [`is_reviewer`] refuses.
pub fn reviewers_in(text: &str) -> Result<Vec<String>, String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let name = part.strip_prefix('@').unwrap_or(part);
            if is_reviewer(name) { Ok(name.to_string()) } else { Err(part.to_string()) }
        })
        .collect()
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
            ..DispatchSettings::default()
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
            ..DispatchSettings::default()
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
    fn each_phase_runs_the_agent_the_project_names_for_it() {
        let settings = DispatchSettings {
            agents: PhaseAgents {
                research: Some("codex".into()),
                implementation: Some("claude-code".into()),
            },
            ..DispatchSettings::default()
        };
        assert_eq!(settings.agent(Round::Research), Some("codex"));
        assert_eq!(settings.agent(Round::Implementation), Some("claude-code"));
        assert_eq!(settings.agent(Round::Review), Some("claude-code"), "review is implementation");
        let unset = DispatchSettings {
            agents: PhaseAgents {
                research: Some(String::new()),
                implementation: None,
            },
            ..DispatchSettings::default()
        };
        assert_eq!(unset.agent(Round::Research), None);
        assert_eq!(unset.agent(Round::Implementation), None);
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
            ..DispatchSettings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(json, r#"{"skills":{"addressReview":"answer-review"},"setupCommand":"npm ci"}"#);
        assert_eq!(serde_json::from_str::<DispatchSettings>(&json).unwrap(), settings);
        let agents = DispatchSettings {
            agents: PhaseAgents {
                implementation: Some("codex".into()),
                ..PhaseAgents::default()
            },
            ..DispatchSettings::default()
        };
        let json = serde_json::to_string(&agents).unwrap();
        assert_eq!(json, r#"{"agents":{"implementation":"codex"}}"#);
        assert_eq!(serde_json::from_str::<DispatchSettings>(&json).unwrap(), agents);
        let reviewers = DispatchSettings {
            reviewers: vec!["ana".into(), "acme/web".into()],
            ..DispatchSettings::default()
        };
        let json = serde_json::to_string(&reviewers).unwrap();
        assert_eq!(json, r#"{"reviewers":["ana","acme/web"]}"#);
        assert_eq!(serde_json::from_str::<DispatchSettings>(&json).unwrap(), reviewers);
    }

    #[test]
    fn reviewers_are_logins_or_teams_of_an_organisation() {
        let long = "x".repeat(39);
        for good in ["ana", "Ana-K", "x1", long.as_str(), "acme/web", "acme/web_team.2"] {
            assert!(is_reviewer(good), "{good}");
        }
        for bad in ["", "@ana", "-ana", "ana k", "ana/", "/web", "a/b/c", "ana!"] {
            assert!(!is_reviewer(bad), "{bad}");
        }
        assert!(!is_reviewer(&"x".repeat(40)));
    }

    #[test]
    fn reviewers_are_read_from_commas_or_spaces_with_or_without_an_at() {
        assert_eq!(reviewers_in(""), Ok(Vec::new()));
        let read = reviewers_in(" @ana, bob  acme/web,,").unwrap();
        assert_eq!(read, ["ana", "bob", "acme/web"]);
        assert_eq!(reviewers_in("ana, b@b"), Err("b@b".to_string()));
    }

    #[test]
    fn a_review_request_asks_people_and_teams_but_never_the_author() {
        let settings = DispatchSettings {
            reviewers: ["ana", "Maui1911", "acme/web", "bob", "ANA", "not a login"]
                .map(String::from)
                .to_vec(),
            ..DispatchSettings::default()
        };
        let request = settings.review_request("maui1911");
        assert_eq!(request.users, ["ana", "bob"]);
        assert_eq!(request.teams, ["acme/web"]);
        assert_eq!(request.names(), "ana, bob and acme/web");
        assert!(!request.is_empty());
        assert!(DispatchSettings::default().review_request("maui1911").is_empty());
        let only_author = DispatchSettings {
            reviewers: vec!["maui1911".into()],
            ..DispatchSettings::default()
        };
        assert!(only_author.review_request("maui1911").is_empty());
    }
}
