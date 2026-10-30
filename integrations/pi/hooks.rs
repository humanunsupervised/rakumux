//! The Rakumux extension for Pi sends activity v1 snapshots itself (and its
//! conversation's folder as `working_directory`): validated, not translated.
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let update: ActivityUpdate =
        serde_json::from_value(input.clone()).map_err(|_| "invalid Pi activity update")?;
    if update.agent != "pi" {
        return Err("activity agent does not match hook adapter".into());
    }
    Ok(Some(update))
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::json;

    fn snapshot(phase: &str) -> Value {
        json!({"version": 1, "agent": "pi", "conversation_id": "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0",
            "event": "snapshot", "phase": phase, "subagents": [],
            "working_directory": {"agent": "pi", "conversation_id": "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0",
                "path": "/repo"}})
    }

    #[test]
    fn pis_snapshots_pass_through_with_their_folder() {
        let mut done = snapshot("done");
        done["completion_id"] = json!("9b5e39f0");
        done["prompt"] = json!("secret prompt");
        let report = report("pi", &done).unwrap().unwrap();
        let activity = report.activity.clone().unwrap();
        assert_eq!(activity.agent, "pi");
        assert_eq!(activity.completion_id.as_deref(), Some("9b5e39f0"));
        assert_eq!(
            activity.event,
            ActivityEvent::Snapshot {
                phase: ActivityPhase::Done,
                subagents: Vec::new()
            }
        );
        assert_eq!(
            report.working_directory.unwrap().path,
            std::path::Path::new("/repo")
        );
        assert!(!serde_json::to_string(&activity).unwrap().contains("secret"));
    }

    #[test]
    fn another_agents_or_a_malformed_update_is_refused() {
        let mut other = snapshot("working");
        other["agent"] = json!("opencode");
        assert!(normalize("pi", &other).is_err());
        let mut elsewhere = snapshot("working");
        elsewhere["working_directory"]["agent"] = json!("claude");
        assert!(report("pi", &elsewhere).is_err());
        assert!(normalize("pi", &json!({"phase": "working"})).is_err());
        // A Pi update reaches no other adapter.
        assert!(normalize("opencode", &snapshot("working")).is_err());
        // And has no hook configuration: it's an extension.
        assert_eq!(config("pi"), None);
    }
}
