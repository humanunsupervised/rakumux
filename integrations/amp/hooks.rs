//! What the Rakumux plugin for Amp reports: a complete activity snapshot of
//! the thread Amp shows (`rakumux-session/activity.mjs`), with Amp's
//! workspace as the working directory. Already metadata only.
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let update: ActivityUpdate =
        serde_json::from_value(input.clone()).map_err(|_| "invalid Amp activity update")?;
    if update.agent != "amp" {
        return Err("activity agent does not match hook adapter".into());
    }
    Ok(Some(update))
}

/// The snapshot's `working_directory`, when it has one: Amp's workspace.
pub(super) fn working_directory(input: &Value) -> Result<Option<AgentWorkingDirectory>, String> {
    let Some(value) = input.get("working_directory") else {
        return Ok(None);
    };
    let context: AgentWorkingDirectory =
        serde_json::from_value(value.clone()).map_err(|_| "invalid Amp working directory")?;
    if context.agent != "amp" {
        return Err("working-directory agent does not match hook adapter".into());
    }
    Ok(Some(context))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_plugins_snapshots_are_reported_as_they_are() {
        let input = json!({"version": 1, "agent": "amp", "event": "snapshot",
            "conversation_id": "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f",
            "phase": "done", "completion_id": "msg-7", "subagents": ["toolu_task"],
            "working_directory": {"agent": "amp",
                "conversation_id": "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f", "path": "/repo"}});
        let report = report("amp", &input).unwrap().unwrap();
        let activity = report.activity.unwrap();
        assert_eq!(activity.completion_id.as_deref(), Some("msg-7"));
        assert_eq!(
            activity.event,
            ActivityEvent::Snapshot {
                phase: ActivityPhase::Done,
                subagents: vec!["toolu_task".into()],
            }
        );
        let directory = report.working_directory.unwrap();
        assert_eq!(directory.path, std::path::Path::new("/repo"));
        assert_eq!(directory.agent, "amp");
        // Without a workspace, activity alone.
        let mut bare = input.clone();
        bare.as_object_mut().unwrap().remove("working_directory");
        assert!(report_without_directory(&bare));
    }

    fn report_without_directory(input: &Value) -> bool {
        let report = report("amp", input).unwrap().unwrap();
        report.activity.is_some() && report.working_directory.is_none()
    }

    #[test]
    fn another_agents_update_or_directory_is_refused() {
        let opencode = json!({"version": 1, "agent": "opencode", "event": "snapshot",
            "conversation_id": "ses_1", "phase": "working", "subagents": []});
        assert!(report("amp", &opencode).is_err());
        let mismatched = json!({"version": 1, "agent": "amp", "event": "snapshot",
            "conversation_id": "T-1", "phase": "working", "subagents": [],
            "working_directory": {"agent": "claude", "path": "/repo"}});
        assert!(report("amp", &mismatched).is_err());
        assert!(report("amp", &json!({"phase": "working"})).is_err());
    }
}
