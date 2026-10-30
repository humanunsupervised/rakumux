//! Lifecycle of a live session: screen detection combined with self-reports.
//!
//! - A fresh self-report (`session.report` with a state) wins for
//!   [`REPORT_FRESH_MS`], then detection takes over again.
//! - `Working → Idle` becomes `Done` and stays `Done` until the session is
//!   touched (seen). A report of `done` has the same effect.
//! - `Unknown` (agent running, state unrecognisable) neither starts nor
//!   cancels a turn.
//! - Fresh hook activity wins over detection, except the agent's own busy
//!   state from command hooks whose agent's screen, recognised as working after
//!   the latest hook, has then looked idle for [`SCREEN_IDLE_MS`]: that turn
//!   ended without a hook (an interrupted Claude Code turn fires no Stop), so
//!   the activity is dropped. An agent waiting for its background agents, and
//!   subagents still running after their parent's turn, look idle on its screen
//!   by design and are never second-guessed.
//! - Without fresh hook activity, what the program says through OSC 7501
//!   (the Program Status Protocol: Pi, Kimi Code) wins over the screen while
//!   detection names an agent; its child records count as subagents. It has
//!   no expiry (the protocol has no heartbeat) and ends with the program's
//!   `clear` or when detection names no agent.
//! - For an agent whose hooks report no approval prompts (Cursor's;
//!   `detect::approvals_on_screen`), a prompt its screen shows (`Blocked`)
//!   wins over the hooks' busy state while it's shown. Other agents' hooks
//!   report their prompts themselves and keep winning.

use super::activity::ActivityTracker;
use super::program_status::ProgramRecords;
use crate::detect::Detection;
use session_core::activity::{ActivityUpdate, AgentActivity};
use session_core::model::Lifecycle;

/// How long a reported state overrides detection.
pub(crate) const REPORT_FRESH_MS: i64 = 30_000;

/// How long the agent's screen must look idle after the latest hook before a
/// busy hook state is considered missed. Longer than a Stop hook takes to land.
pub(crate) const SCREEN_IDLE_MS: i64 = 5_000;

#[derive(Debug, Clone)]
pub(crate) struct LifecycleTracker {
    detected: Lifecycle,
    /// When detection last changed its agent or lifecycle (ms).
    detected_since: i64,
    /// When detection last said `Working` (ms).
    working_seen: Option<i64>,
    agent: Option<String>,
    /// Last reported state (never `Done`; see `on_report`) and when.
    report: Option<(Lifecycle, i64)>,
    activity: Option<ActivityTracker>,
    /// What the program says through OSC 7501, while detection names an agent.
    program: Option<ProgramRecords>,
    /// A turn is in progress (Working seen since the last Idle).
    in_turn: bool,
    /// A turn finished and nobody has looked yet.
    done: bool,
    current: Lifecycle,
}

impl Default for LifecycleTracker {
    fn default() -> Self {
        Self {
            detected: Lifecycle::None,
            detected_since: 0,
            working_seen: None,
            agent: None,
            report: None,
            activity: None,
            program: None,
            in_turn: false,
            done: false,
            current: Lifecycle::None,
        }
    }
}

impl LifecycleTracker {
    pub fn current(&self) -> Lifecycle {
        self.current
    }

    /// Agent kind from fresh hooks, otherwise screen/process detection.
    pub fn detected_agent(&self) -> Option<&str> {
        self.activity
            .as_ref()
            .map(|activity| activity.agent.as_str())
            .or(self.agent.as_deref())
    }

    /// Whether `update` is the heartbeat of a subagent the activity counted:
    /// it comes from that subagent's own conversation.
    pub fn is_subagent_update(&self, update: &ActivityUpdate) -> bool {
        self.activity
            .as_ref()
            .is_some_and(|activity| activity.is_subagent(update))
    }

    /// The conversation `agent`'s hook activity is about, while it has some.
    pub fn hook_conversation(&self, agent: &str) -> Option<&str> {
        self.activity
            .as_ref()
            .filter(|activity| activity.agent == agent)
            .map(|activity| activity.conversation())
    }

    /// The hook activity, else what the program says through OSC 7501.
    pub fn activity(&self) -> Option<AgentActivity> {
        self.activity
            .as_ref()
            .map(ActivityTracker::view)
            .or_else(|| self.program().map(ProgramRecords::view))
    }

    /// The OSC 7501 records, while detection names an agent.
    fn program(&self) -> Option<&ProgramRecords> {
        self.program.as_ref().filter(|_| self.agent.is_some())
    }

    /// The program reported what it is doing (OSC 7501). Returns true when
    /// the lifecycle changed.
    pub fn on_program_status(&mut self, status: &vt::ProgramStatus, now_ms: i64) -> bool {
        let records = self.program.get_or_insert_with(ProgramRecords::default);
        let finished = records.apply(status, now_ms);
        if records.is_empty() {
            self.program = None;
        }
        // Hooks, when they report, say when a turn finished themselves.
        if finished && self.activity.is_none() && self.agent.is_some() {
            self.done = true;
            self.in_turn = false;
        }
        self.recompute(now_ms)
    }

    pub fn activity_handoff(&self) -> Option<(ActivityUpdate, i64)> {
        self.activity.as_ref().map(ActivityTracker::handoff)
    }

    pub fn on_activity(&mut self, update: &ActivityUpdate, now: i64) {
        // This hook was received after any earlier self-report, even when
        // both arrived in the same millisecond.
        self.report = None;
        if self
            .activity
            .as_ref()
            .is_some_and(|activity| !activity.accepts(update))
        {
            self.done = false;
            self.in_turn = false;
        }
        let activity = self
            .activity
            .get_or_insert_with(|| ActivityTracker::new(update, now));
        if activity.apply(update, now) {
            self.done = true;
            self.in_turn = false;
        }
        self.recompute(now);
    }

    /// New detection result. Returns true when the lifecycle changed.
    pub fn on_detection(&mut self, detection: Detection, now_ms: i64) -> bool {
        if self
            .activity
            .as_ref()
            .is_some_and(|activity| detection.agent.as_ref() != Some(&activity.agent))
        {
            self.activity = None;
        }
        let detected = match detection.lifecycle {
            // Detection never reports Done; treat one as Idle defensively.
            Lifecycle::Done => Lifecycle::Idle,
            other => other,
        };
        if detected != self.detected || detection.agent != self.agent {
            self.detected_since = now_ms;
        }
        if detected == Lifecycle::Working {
            self.working_seen = Some(now_ms);
        }
        self.agent = detection.agent;
        self.detected = detected;
        // The program that reported is gone (a shell, or nothing, again).
        if self.agent.is_none() {
            self.program = None;
        }
        if self.hooks_contradicted(now_ms) {
            self.activity = None;
        }
        self.recompute(now_ms)
    }

    /// The session reported `state` about itself.
    pub fn on_report(&mut self, state: Lifecycle, now_ms: i64) -> bool {
        let state = if state == Lifecycle::Done {
            self.done = true;
            self.in_turn = false;
            Lifecycle::Idle
        } else {
            state
        };
        self.report = Some((state, now_ms));
        self.recompute(now_ms)
    }

    /// The session was seen: `Done` turns into `Idle`.
    pub fn on_touch(&mut self, now_ms: i64) -> bool {
        self.done = false;
        self.recompute(now_ms)
    }

    /// Periodic re-evaluation (self-report and hook expiry, missed turn ends).
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let stale = self
            .activity
            .as_ref()
            .is_some_and(|activity| !activity.fresh(now_ms))
            || self.hooks_contradicted(now_ms);
        if stale {
            self.activity = None;
        }
        self.recompute(now_ms) || stale
    }

    /// The agent's own busy state from command hooks (working or running a tool,
    /// its subagents' included) whose agent's screen was recognised as working
    /// after the latest hook (so its markers work for this version) and has since
    /// looked idle for [`SCREEN_IDLE_MS`]. Waiting-for-input, waiting-for-agents
    /// and finished phases, subagents running past their parent's turn, and
    /// live-cache snapshots are never second-guessed; nor is a screen never seen
    /// busy.
    fn hooks_contradicted(&self, now_ms: i64) -> bool {
        use session_core::activity::ActivityPhase;
        let Some(activity) = &self.activity else {
            return false;
        };
        let busy = matches!(
            activity.view().phase,
            ActivityPhase::Working | ActivityPhase::WaitingForResponse | ActivityPhase::RunningTool
        );
        busy && activity.from_deltas()
            && self.detected == Lifecycle::Idle
            && self.agent.as_deref() == Some(activity.agent.as_str())
            && self
                .working_seen
                .is_some_and(|seen| seen >= activity.received_ms())
            && now_ms.saturating_sub(self.detected_since) >= SCREEN_IDLE_MS
    }

    /// `agent`'s screen shows an approval prompt its hooks can't report
    /// (`detect::approvals_on_screen`: Cursor's).
    fn approval_on_screen(&self, agent: &str) -> bool {
        self.detected == Lifecycle::Blocked
            && self.agent.as_deref() == Some(agent)
            && crate::detect::approvals_on_screen(agent)
    }

    fn base(&self, now_ms: i64) -> Lifecycle {
        if let Some(activity) = self
            .activity
            .as_ref()
            .filter(|activity| activity.fresh(now_ms))
        {
            if let Some((state, received_ms)) = self.report {
                if received_ms >= activity.received_ms()
                    && now_ms.saturating_sub(received_ms) < REPORT_FRESH_MS
                {
                    return state;
                }
            }
            let view = activity.view();
            let hooked = if view.active_subagents > 0
                && view.phase != session_core::activity::ActivityPhase::NeedsInput
            {
                Lifecycle::Working
            } else {
                view.phase.lifecycle()
            };
            return if hooked == Lifecycle::Working && self.approval_on_screen(&activity.agent) {
                Lifecycle::Blocked
            } else {
                hooked
            };
        }
        let fresh_report = self
            .report
            .filter(|(_, at)| now_ms.saturating_sub(*at) < REPORT_FRESH_MS);
        if let Some((lifecycle, received_ms)) = self
            .program()
            .and_then(|program| Some((program.lifecycle()?, program.received_ms())))
        {
            return match fresh_report {
                Some((state, at)) if at >= received_ms => state,
                _ => lifecycle,
            };
        }
        match fresh_report {
            Some((state, _)) => state,
            None => self.detected,
        }
    }

    fn recompute(&mut self, now_ms: i64) -> bool {
        let base = self.base(now_ms);
        match base {
            Lifecycle::Working => {
                self.in_turn = true;
                self.done = false;
            }
            Lifecycle::Blocked | Lifecycle::None => {
                self.in_turn = false;
                self.done = false;
            }
            Lifecycle::Idle => {
                if self.in_turn {
                    self.done = true;
                }
                self.in_turn = false;
            }
            Lifecycle::Unknown | Lifecycle::Done => {}
        }
        let next = if base == Lifecycle::Idle && self.done {
            Lifecycle::Done
        } else {
            base
        };
        let changed = next != self.current;
        self.current = next;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(lifecycle: Lifecycle) -> Detection {
        Detection {
            agent: Some("claude".into()),
            lifecycle,
        }
    }

    #[test]
    fn working_then_idle_is_done_until_touched() {
        let mut t = LifecycleTracker::default();
        assert_eq!(t.current(), Lifecycle::None);
        assert!(t.on_detection(detection(Lifecycle::Idle), 0));
        assert_eq!(t.current(), Lifecycle::Idle);
        assert!(t.on_detection(detection(Lifecycle::Working), 1));
        assert!(t.on_detection(detection(Lifecycle::Idle), 2));
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(!t.on_detection(detection(Lifecycle::Idle), 3));
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(t.on_touch(4));
        assert_eq!(t.current(), Lifecycle::Idle);
        assert_eq!(t.detected_agent(), Some("claude"));
    }

    #[test]
    fn unknown_does_not_break_a_turn_and_blocked_cancels_it() {
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_detection(detection(Lifecycle::Unknown), 1);
        t.on_detection(detection(Lifecycle::Idle), 2);
        assert_eq!(t.current(), Lifecycle::Done);

        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_detection(detection(Lifecycle::Blocked), 1);
        t.on_detection(detection(Lifecycle::Idle), 2);
        assert_eq!(t.current(), Lifecycle::Idle);
    }

    #[test]
    fn fresh_reports_win_then_expire() {
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Idle), 0);
        assert!(t.on_report(Lifecycle::Working, 1_000));
        assert_eq!(t.current(), Lifecycle::Working);
        assert!(!t.on_detection(detection(Lifecycle::Idle), 2_000));
        assert_eq!(t.current(), Lifecycle::Working);
        // Report expired: detection says Idle after a Working turn.
        assert!(t.tick(1_000 + REPORT_FRESH_MS));
        assert_eq!(t.current(), Lifecycle::Done);
    }

    #[test]
    fn reported_done_is_cleared_by_touch_while_still_fresh() {
        let mut t = LifecycleTracker::default();
        t.on_report(Lifecycle::Working, 0);
        assert!(t.on_report(Lifecycle::Done, 10));
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(t.on_touch(20));
        assert_eq!(t.current(), Lifecycle::Idle);
        assert!(!t.tick(30));
        assert_eq!(t.current(), Lifecycle::Idle);
    }

    #[test]
    fn activity_overrides_stale_screens_then_expires_or_clears_on_agent_exit() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        let mut tracker = LifecycleTracker::default();
        tracker.on_detection(detection(Lifecycle::Idle), 0);
        let update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "root".into(),
            completion_id: None,
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            },
        };
        tracker.on_activity(&update, 1);
        tracker.on_detection(detection(Lifecycle::Idle), 2);
        assert_eq!(tracker.current(), Lifecycle::Working);
        assert!(tracker.activity().is_some());
        assert!(tracker.tick(super::super::activity::FRESH_MS + 1));
        assert!(tracker.activity().is_none());
        tracker.on_activity(&update, 200_000);
        tracker.on_detection(
            Detection {
                agent: None,
                lifecycle: Lifecycle::None,
            },
            200_001,
        );
        assert_eq!(tracker.current(), Lifecycle::None);
        assert!(tracker.activity().is_none());
    }

    #[test]
    fn a_newer_explicit_report_wins_until_a_later_hook_update() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        let mut tracker = LifecycleTracker::default();
        let update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "root".into(),
            completion_id: None,
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            },
        };
        tracker.on_activity(&update, 1);
        tracker.on_report(Lifecycle::Done, 2);
        assert_eq!(tracker.current(), Lifecycle::Done);
        tracker.on_activity(&update, 2);
        assert_eq!(tracker.current(), Lifecycle::Working);
    }

    #[test]
    fn completed_reply_stays_seen_across_hook_heartbeats_until_a_new_completion() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        for agent in ["claude", "codex", "opencode"] {
            let mut tracker = LifecycleTracker::default();
            let mut update = ActivityUpdate {
                version: ACTIVITY_VERSION,
                agent: agent.into(),
                conversation_id: "root".into(),
                completion_id: Some("reply-1".into()),
                event: ActivityEvent::Snapshot {
                    phase: ActivityPhase::Done,
                    subagents: vec![],
                },
            };
            tracker.on_activity(&update, 1);
            assert_eq!(tracker.current(), Lifecycle::Done);
            tracker.on_touch(2);
            tracker.on_activity(&update, 3);
            assert_eq!(tracker.current(), Lifecycle::Idle);
            update.completion_id = Some("reply-2".into());
            tracker.on_activity(&update, 4);
            assert_eq!(tracker.current(), Lifecycle::Done);
        }
    }

    #[test]
    fn native_stop_duplicates_do_not_resurrect_a_seen_reply() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        let mut tracker = LifecycleTracker::default();
        let mut update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "root".into(),
            completion_id: None,
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Done,
            },
        };
        tracker.on_activity(&update, 1);
        tracker.on_touch(2);
        tracker.on_activity(&update, 3);
        assert_eq!(tracker.current(), Lifecycle::Idle);
        update.event = ActivityEvent::Phase {
            phase: ActivityPhase::Working,
        };
        tracker.on_activity(&update, 4);
        update.event = ActivityEvent::Phase {
            phase: ActivityPhase::Done,
        };
        tracker.on_activity(&update, 5);
        assert_eq!(tracker.current(), Lifecycle::Done);
    }

    #[test]
    fn switching_conversations_does_not_transfer_an_unread_reply_or_in_progress_turn() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        for phase in [ActivityPhase::Done, ActivityPhase::Working] {
            let mut tracker = LifecycleTracker::default();
            let mut update = ActivityUpdate {
                version: ACTIVITY_VERSION,
                agent: "opencode".into(),
                conversation_id: "old".into(),
                completion_id: None,
                event: ActivityEvent::Phase { phase },
            };
            tracker.on_activity(&update, 1);
            update.conversation_id = "new".into();
            update.event = ActivityEvent::Phase {
                phase: ActivityPhase::Idle,
            };
            tracker.on_activity(&update, 2);
            assert_eq!(tracker.current(), Lifecycle::Idle);
        }
    }

    fn hook(event: session_core::activity::ActivityEvent) -> ActivityUpdate {
        ActivityUpdate {
            version: session_core::activity::ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "root".into(),
            completion_id: None,
            event,
        }
    }

    fn phase(phase: session_core::activity::ActivityPhase) -> ActivityUpdate {
        hook(session_core::activity::ActivityEvent::Phase { phase })
    }

    #[test]
    fn an_idle_screen_after_the_last_hook_ends_a_turn_whose_end_fired_no_hook() {
        use session_core::activity::ActivityPhase;
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_activity(&phase(ActivityPhase::RunningTool), 1_000);
        t.on_detection(detection(Lifecycle::Working), 2_000);
        // Esc: Claude Code redraws its idle prompt and sends no Stop.
        t.on_detection(detection(Lifecycle::Idle), 3_000);
        assert!(!t.tick(3_000 + SCREEN_IDLE_MS - 1));
        assert_eq!(t.current(), Lifecycle::Working, "a Stop may still land");
        assert!(t.tick(3_000 + SCREEN_IDLE_MS));
        assert!(t.activity().is_none(), "the missed hook state is dropped");
        assert_eq!(t.current(), Lifecycle::Done);
        // The next hook starts tracking again.
        t.on_activity(&phase(ActivityPhase::Working), 10_000);
        assert_eq!(t.current(), Lifecycle::Working);
    }

    #[test]
    fn hook_states_the_screen_cannot_contradict_are_kept() {
        use session_core::activity::{ActivityEvent, ActivityPhase};
        // The screen was already idle when the hook arrived (no redraw since).
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Idle), 0);
        t.on_activity(&phase(ActivityPhase::Working), 1_000);
        t.on_detection(detection(Lifecycle::Idle), 2_000);
        assert!(!t.tick(60_000));
        assert_eq!(t.current(), Lifecycle::Working);

        // A screen never recognised as busy after the hook proves nothing (its
        // markers may not match this agent version).
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_activity(&phase(ActivityPhase::Working), 1_000);
        t.on_detection(detection(Lifecycle::Idle), 2_000);
        t.tick(60_000);
        assert_eq!(t.current(), Lifecycle::Working);

        // Waiting for the user and live-cache snapshots are never second-guessed.
        for update in [
            phase(ActivityPhase::NeedsInput),
            hook(ActivityEvent::Snapshot {
                phase: ActivityPhase::Working,
                subagents: vec![],
            }),
        ] {
            let mut t = LifecycleTracker::default();
            t.on_activity(&update, 1_000);
            t.on_detection(detection(Lifecycle::Working), 1_500);
            t.on_detection(detection(Lifecycle::Idle), 2_000);
            t.tick(60_000);
            assert!(t.activity().is_some(), "{update:?}");
        }

        // A hook after the screen went idle wins again.
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_activity(&phase(ActivityPhase::Working), 1_000);
        t.on_detection(detection(Lifecycle::Idle), 2_000);
        t.on_activity(&phase(ActivityPhase::RunningTool), 3_000);
        t.tick(60_000);
        assert_eq!(t.current(), Lifecycle::Working);
    }

    #[test]
    fn subagent_heartbeats_keep_a_background_subagent_counted_after_its_parent_waits() {
        use super::super::activity::FRESH_MS;
        use session_core::activity::{ActivityEvent, ActivityPhase};
        let child = |running| {
            hook(ActivityEvent::Subagent {
                id: "child".into(),
                running,
            })
        };
        let mut t = LifecycleTracker::default();
        t.on_activity(&phase(ActivityPhase::RunningTool), 0);
        t.on_activity(&child(true), 1);
        // Claude's Stop while a background agent runs: waiting for it.
        t.on_activity(&phase(ActivityPhase::WaitingForAgents), 2);
        // The child's own tool hooks are its heartbeat, beyond the parent's freshness.
        for at in [FRESH_MS / 2, FRESH_MS, FRESH_MS * 3 / 2] {
            t.on_activity(&child(true), at);
            assert!(!t.tick(at + 1));
        }
        assert_eq!(t.activity().unwrap().active_subagents, 1);
        assert_eq!(t.current(), Lifecycle::Working);
        t.on_activity(&child(false), FRESH_MS * 2);
        t.on_activity(&child(true), FRESH_MS * 2 + 1);
        assert_eq!(
            t.activity().unwrap().active_subagents,
            0,
            "a late hook cannot resurrect a stopped subagent"
        );
        t.on_activity(&phase(ActivityPhase::Done), FRESH_MS * 2 + 2);
        assert_eq!(t.current(), Lifecycle::Done);
        // idle_prompt about a minute later: the unread reply stays unread.
        t.on_activity(&phase(ActivityPhase::Idle), FRESH_MS * 2 + 60_000);
        assert_eq!(t.current(), Lifecycle::Done);
    }

    #[test]
    fn waiting_for_background_agents_stays_working_while_the_screen_waits_for_input() {
        use super::super::activity::FRESH_MS;
        use session_core::activity::{ActivityEvent, ActivityPhase};
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_activity(&phase(ActivityPhase::RunningTool), 500);
        // Stop with a background agent in flight, the spinner still drawn just after.
        t.on_activity(&phase(ActivityPhase::WaitingForAgents), 1_000);
        t.on_detection(detection(Lifecycle::Working), 1_100);
        t.on_detection(detection(Lifecycle::Idle), 1_200);
        // The idle screen doesn't end it, nor do minutes without a hook (the
        // agent is in one long tool call).
        for at in [
            1_200 + SCREEN_IDLE_MS + 1,
            1_000 + FRESH_MS + 1,
            1_000 + 5 * 60_000,
        ] {
            t.tick(at);
            assert_eq!(t.current(), Lifecycle::Working, "{at}");
        }
        // Its tool hooks count it; when it's done the parent wakes and finishes.
        let child = |running| {
            hook(ActivityEvent::Subagent {
                id: "child".into(),
                running,
            })
        };
        t.on_activity(&child(true), 400_000);
        assert_eq!(t.activity().unwrap().active_subagents, 1);
        t.on_activity(&child(false), 410_000);
        assert_eq!(t.current(), Lifecycle::Working);
        t.on_activity(&phase(ActivityPhase::Done), 420_000);
        assert_eq!(t.current(), Lifecycle::Done);
    }

    #[test]
    fn an_approval_on_screen_wins_over_busy_hooks_only_for_agents_whose_hooks_miss_it() {
        use session_core::activity::{ActivityEvent, ActivityPhase};
        let seen = |agent: &str, event: ActivityEvent| {
            let on_screen = |lifecycle| Detection {
                agent: Some(agent.into()),
                lifecycle,
            };
            let mut t = LifecycleTracker::default();
            t.on_detection(on_screen(Lifecycle::Working), 0);
            t.on_activity(
                &ActivityUpdate {
                    agent: agent.into(),
                    ..hook(event.clone())
                },
                1_000,
            );
            // Cursor's preToolUse, then the approval box it draws.
            t.on_detection(on_screen(Lifecycle::Blocked), 1_100);
            let blocked = t.current();
            t.on_detection(on_screen(Lifecycle::Working), 5_000);
            (blocked, t.current())
        };
        let running_tool = ActivityEvent::Phase {
            phase: ActivityPhase::RunningTool,
        };
        let subagent = ActivityEvent::Subagent {
            id: "child".into(),
            running: true,
        };
        for event in [running_tool.clone(), subagent.clone()] {
            assert_eq!(
                seen("cursor-agent", event),
                (Lifecycle::Blocked, Lifecycle::Working),
                "approved: busy again"
            );
        }
        // Claude Code, Codex and OpenCode report their prompts in hooks: the
        // hooks keep winning, as before.
        for agent in ["claude", "codex", "opencode"] {
            for event in [running_tool.clone(), subagent.clone()] {
                assert_eq!(
                    seen(agent, event),
                    (Lifecycle::Working, Lifecycle::Working),
                    "{agent}"
                );
            }
        }
    }

    fn program(state: vt::ProgramState, id: Option<&str>) -> vt::ProgramStatus {
        vt::ProgramStatus {
            state,
            kind: None,
            id: id.map(String::from),
            app: Some("pi".into()),
        }
    }

    #[test]
    fn what_the_program_says_through_osc_7501_wins_over_its_screen() {
        use session_core::activity::ActivityPhase;
        use vt::ProgramState::*;
        let pi = |lifecycle| Detection {
            agent: Some("pi".into()),
            lifecycle,
        };
        let mut t = LifecycleTracker::default();
        t.on_detection(pi(Lifecycle::Unknown), 0);
        t.on_program_status(&program(Idle, None), 1);
        assert_eq!(t.current(), Lifecycle::Idle);
        // Pi 1.4's screen looks idle while it works: the report wins.
        t.on_program_status(&program(Working, None), 2);
        t.on_detection(pi(Lifecycle::Idle), 3);
        assert_eq!(t.current(), Lifecycle::Working);
        assert_eq!(t.activity().unwrap().phase, ActivityPhase::Working);
        t.on_program_status(&program(Done, None), 4);
        assert_eq!(t.current(), Lifecycle::Done, "an unread reply");
        t.on_touch(5);
        assert_eq!(t.current(), Lifecycle::Idle);
        t.on_program_status(&program(Done, None), 6);
        assert_eq!(t.current(), Lifecycle::Idle, "the same done again");
        // No expiry: the protocol has no heartbeat.
        t.on_program_status(&program(Working, None), 7);
        assert!(!t.tick(10 * 60_000));
        assert_eq!(t.current(), Lifecycle::Working);
        // Blocked on the user (a dialog).
        t.on_program_status(&program(Blocked, None), 10 * 60_000 + 1);
        assert_eq!(t.current(), Lifecycle::Blocked);
        assert_eq!(t.activity().unwrap().phase, ActivityPhase::NeedsInput);
        // A fresh self-report sent after it still wins.
        t.on_report(Lifecycle::Working, 10 * 60_000 + 2);
        assert_eq!(t.current(), Lifecycle::Working);
        // The program leaves: the screen again.
        t.on_program_status(&program(Clear, None), 10 * 60_000 + 3);
        t.tick(10 * 60_000 + REPORT_FRESH_MS + 3);
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(t.activity().is_none());
    }

    #[test]
    fn child_records_count_as_subagents_and_hooks_still_rank_first() {
        use session_core::activity::ActivityPhase;
        use vt::ProgramState::*;
        let kimi = |lifecycle| Detection {
            agent: Some("kimi".into()),
            lifecycle,
        };
        let mut t = LifecycleTracker::default();
        t.on_detection(kimi(Lifecycle::Idle), 0);
        t.on_program_status(&program(Working, None), 1);
        t.on_program_status(&program(Working, Some("agent-1")), 2);
        t.on_program_status(&program(Working, Some("agent-2")), 3);
        assert_eq!(t.activity().unwrap().active_subagents, 2);
        t.on_program_status(&program(Clear, Some("agent-2")), 4);
        t.on_program_status(&program(Done, None), 5);
        // A background subagent past the program's own turn: still working.
        assert_eq!(t.current(), Lifecycle::Working);
        assert_eq!(t.activity().unwrap().phase, ActivityPhase::WaitingForAgents);
        t.on_program_status(&program(Done, Some("agent-1")), 6);
        assert_eq!(t.current(), Lifecycle::Done);
        // Fresh hook activity ranks above the records.
        t.on_activity(
            &ActivityUpdate {
                agent: "kimi".into(),
                ..phase(ActivityPhase::NeedsInput)
            },
            7,
        );
        assert_eq!(t.current(), Lifecycle::Blocked);
        t.on_program_status(&program(Working, None), 8);
        assert_eq!(t.current(), Lifecycle::Blocked);
        assert_eq!(t.activity().unwrap().phase, ActivityPhase::NeedsInput);
    }

    #[test]
    fn program_records_need_an_agent_and_end_with_it() {
        use vt::ProgramState::*;
        let mut t = LifecycleTracker::default();
        let none = Detection {
            agent: None,
            lifecycle: Lifecycle::None,
        };
        // A program detection doesn't name: its records say nothing.
        t.on_detection(none.clone(), 0);
        t.on_program_status(&program(Working, None), 1);
        assert_eq!(t.current(), Lifecycle::None);
        assert!(t.activity().is_none());
        // Reported, then named (the report arrives before detection runs).
        t.on_detection(
            Detection {
                agent: Some("pi".into()),
                lifecycle: Lifecycle::Idle,
            },
            2,
        );
        t.on_program_status(&program(Working, None), 3);
        assert_eq!(t.current(), Lifecycle::Working);
        // It crashed back to the shell without its clear.
        t.on_detection(none, 4);
        assert_eq!(t.current(), Lifecycle::None);
        t.on_detection(
            Detection {
                agent: Some("claude".into()),
                lifecycle: Lifecycle::Idle,
            },
            5,
        );
        assert_eq!(t.current(), Lifecycle::Idle, "nothing carried over");
    }

    #[test]
    fn subagents_running_after_their_parents_turn_are_not_second_guessed() {
        use session_core::activity::{ActivityEvent, ActivityPhase};
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_activity(&phase(ActivityPhase::Done), 1_000);
        t.on_activity(
            &hook(ActivityEvent::Subagent {
                id: "teammate".into(),
                running: true,
            }),
            1_100,
        );
        t.on_detection(detection(Lifecycle::Working), 1_200);
        t.on_detection(detection(Lifecycle::Idle), 1_300);
        t.tick(1_300 + SCREEN_IDLE_MS + 1);
        assert_eq!(t.current(), Lifecycle::Working);
        assert_eq!(t.activity().unwrap().active_subagents, 1);
    }

    #[test]
    fn an_interrupted_turn_with_a_foreground_subagent_still_ends() {
        use session_core::activity::{ActivityEvent, ActivityPhase};
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        // The agent runs its Agent tool; the subagent works under it.
        t.on_activity(&phase(ActivityPhase::RunningTool), 1_000);
        t.on_activity(
            &hook(ActivityEvent::Subagent {
                id: "child".into(),
                running: true,
            }),
            1_100,
        );
        t.on_detection(detection(Lifecycle::Working), 1_200);
        // Esc: no Stop, no SubagentStop; the screen goes idle.
        t.on_detection(detection(Lifecycle::Idle), 1_300);
        t.tick(1_300 + SCREEN_IDLE_MS + 1);
        assert!(t.activity().is_none());
        assert_eq!(t.current(), Lifecycle::Done);
    }
}
