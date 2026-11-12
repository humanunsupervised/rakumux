//! `rakumux heartbeat`: tasks the daemon runs on a clock (`ls`, `show`,
//! `add`, `update`, `enable`, `pause`, `run`, `rm`). From inside a session
//! (an agent) `add` and `update` work too, and what they make or change waits
//! for the user's approval; `enable`, `run` and `rm` are the user's.

use super::args::{HeartbeatAddArgs, HeartbeatCommand, HeartbeatUpdateArgs, ScheduleArgs};
use super::table::{self, ago, Column};
use super::{output_error, CliError, Invocation};
use crate::printable::{printable, printable_lines};
use serde_json::json;
use session_core::api::{ApiError, ErrorCode};
use session_core::client::SessionApi;
use session_core::heartbeat::{
    Heartbeat, HeartbeatEdit, HeartbeatFields, HeartbeatSpec, Schedule, Weekday,
};
use session_core::ids::HeartbeatId;
use session_core::model::ScopeRoot;
use std::io::{Read, Write};

pub(crate) fn run(
    api: &dyn SessionApi,
    invocation: &Invocation,
    command: HeartbeatCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    match command {
        HeartbeatCommand::Ls { json, folder, all } => {
            let scope = match (folder, all) {
                (_, true) => None,
                (Some(dir), false) => Some(invocation.folder_root(&dir)?),
                (None, false) => Some(here(api, invocation)?),
            };
            ls(api, scope.as_ref(), json, out)
        }
        HeartbeatCommand::Show { target, json } => {
            let heartbeat = find(api, invocation, &target)?;
            show(&heartbeat, json, out)
        }
        HeartbeatCommand::Add(args) => add(api, invocation, args, out),
        HeartbeatCommand::Update(args) => update(api, invocation, args, out),
        HeartbeatCommand::Enable { target } => {
            let heartbeat = find(api, invocation, &target)?;
            let heartbeat = api.heartbeat_enable(&heartbeat.id, true)?;
            done(out, &heartbeat, "runs on its schedule")
        }
        HeartbeatCommand::Pause { target } => {
            let heartbeat = find(api, invocation, &target)?;
            let heartbeat = api.heartbeat_enable(&heartbeat.id, false)?;
            done(out, &heartbeat, "is paused")
        }
        HeartbeatCommand::Run { target } => {
            let heartbeat = find(api, invocation, &target)?;
            let session = api.heartbeat_run(&heartbeat.id)?;
            writeln!(out, "Started {} ({})", printable(&session.name), session.id)
                .map_err(output_error)?;
            Ok(0)
        }
        HeartbeatCommand::Rm { target } => {
            let heartbeat = find(api, invocation, &target)?;
            api.heartbeat_delete(&heartbeat.id)?;
            writeln!(
                out,
                "Deleted heartbeat {} (its runs stay as sessions)",
                printable(&heartbeat.name)
            )
            .map_err(output_error)?;
            Ok(0)
        }
    }
}

/// The folder this command runs in: the current directory's for the user,
/// the session's own for an agent.
fn here(api: &dyn SessionApi, invocation: &Invocation) -> Result<ScopeRoot, CliError> {
    invocation.display_scope(api)
}

fn ls(
    api: &dyn SessionApi,
    scope: Option<&ScopeRoot>,
    json: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let heartbeats = api.heartbeats(scope)?;
    if json {
        writeln!(out, "{}", json!({ "heartbeats": heartbeats })).map_err(output_error)?;
        return Ok(0);
    }
    let now = table::now();
    let cells = |cell: &dyn Fn(&Heartbeat) -> String| heartbeats.iter().map(cell).collect();
    let mut columns = vec![Column::new("NAME", cells(&|h| h.name.clone()))];
    if scope.is_none() {
        columns.push(Column::new(
            "FOLDER",
            cells(&|h| h.scope.path().display().to_string()),
        ));
    }
    columns.extend([
        Column::new(
            "SCHEDULE",
            cells(&|h| h.schedule.as_ref().map_or("-".into(), Schedule::to_string)),
        ),
        Column::new("STATE", cells(&|h| state(h).into())),
        Column::new(
            "NEXT",
            cells(&|h| h.next_run_at.map_or("-".into(), |at| until(now, at))),
        ),
        Column::new("LAST", cells(&|h| last(now, h))),
        Column::new("ID", cells(&|h| h.handle())),
    ]);
    out.write_all(table::render(columns).as_bytes())
        .map_err(output_error)?;
    Ok(0)
}

fn show(heartbeat: &Heartbeat, json: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    if json {
        writeln!(out, "{}", json!({ "heartbeat": heartbeat })).map_err(output_error)?;
        return Ok(0);
    }
    let now = table::now();
    let schedule = heartbeat
        .schedule
        .as_ref()
        .map_or("-".into(), Schedule::to_string);
    let mut text = format!(
        "{}  {}\nfolder:   {}\nstate:    {}\nschedule: {schedule}\nnext:     {}\nlast:     {}\nruns:     {}\ncommand:  {}\nprompt:\n",
        heartbeat.name,
        heartbeat.handle(),
        heartbeat.scope.path().display(),
        state(heartbeat),
        heartbeat.next_run_at.map_or("-".into(), |at| until(now, at)),
        last(now, heartbeat),
        heartbeat.runs,
        if heartbeat.command.is_empty() { "-" } else { &heartbeat.command },
    );
    let prompt = match heartbeat.prompt.trim() {
        "" => "  -".to_string(),
        prompt => prompt
            .lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    text.push_str(&prompt);
    text.push('\n');
    out.write_all(printable_lines(&text).as_bytes())
        .map_err(output_error)?;
    Ok(0)
}

fn add(
    api: &dyn SessionApi,
    invocation: &Invocation,
    args: HeartbeatAddArgs,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let scope = match &args.folder {
        Some(dir) => Some(invocation.folder_root(dir)?),
        None => invocation.request_scope(api)?,
    };
    let fields = HeartbeatFields {
        agent: args.agent,
        command: args.command,
        prompt: prompt(args.prompt, &args.words)?,
        schedule: schedule(&args.schedule)?,
        by_agent: false,
    };
    let complete = fields.prompt.is_some() && fields.schedule.is_some();
    let heartbeat = api.heartbeat_create(HeartbeatSpec {
        scope,
        name: args.name,
        fields,
        enabled: complete && !args.paused,
    })?;
    if args.json {
        return show(&heartbeat, true, out);
    }
    done(out, &heartbeat, "made")
}

fn update(
    api: &dyn SessionApi,
    invocation: &Invocation,
    args: HeartbeatUpdateArgs,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let found = find(api, invocation, &args.target)?;
    let edit = HeartbeatEdit {
        id: found.id,
        name: args.name,
        fields: HeartbeatFields {
            agent: args.agent,
            command: args.command,
            prompt: prompt(args.prompt, &args.words)?,
            schedule: schedule(&args.schedule)?,
            by_agent: false,
        },
    };
    let heartbeat = api.heartbeat_update(edit)?;
    if args.json {
        return show(&heartbeat, true, out);
    }
    done(out, &heartbeat, "changed")
}

/// One line: what happened, and what the heartbeat does now.
fn done(out: &mut dyn Write, heartbeat: &Heartbeat, what: &str) -> Result<i32, CliError> {
    let then = match (
        heartbeat.enabled,
        heartbeat.awaiting_approval,
        heartbeat.is_complete(),
    ) {
        (true, _, _) => format!(
            "; next run {}",
            heartbeat
                .next_run_at
                .map_or("-".into(), |at| until(table::now(), at))
        ),
        (false, true, _) => format!(
            "; it waits for approval: `rakumux heartbeat enable {}`",
            heartbeat.handle()
        ),
        (false, false, false) => "; it runs once it has a prompt and a schedule".into(),
        (false, false, true) => "; paused".into(),
    };
    writeln!(
        out,
        "Heartbeat {} ({}) {what}{then}",
        printable(&heartbeat.name),
        heartbeat.handle()
    )
    .map_err(output_error)?;
    Ok(0)
}

/// `on`, `paused`, `awaiting approval` or `incomplete`.
fn state(heartbeat: &Heartbeat) -> &'static str {
    match (heartbeat.enabled, heartbeat.awaiting_approval) {
        (true, _) => "on",
        (false, true) => "awaiting approval",
        (false, false) if !heartbeat.is_complete() => "incomplete",
        (false, false) => "paused",
    }
}

/// The last run: how long ago and what became of it.
fn last(now: i64, heartbeat: &Heartbeat) -> String {
    match &heartbeat.last_run {
        Some(run) => {
            let number = match run.run {
                0 => String::new(),
                n => format!("#{n} "),
            };
            format!("{number}{} {}", run.outcome.label(), ago(now, run.at))
        }
        None => "-".into(),
    }
}

/// `in 25m`, `in 3h`, `in 2d`, or `now`.
fn until(now: i64, at: i64) -> String {
    let seconds = at - now;
    match seconds {
        s if s <= 0 => "now".into(),
        s if s < 60 => format!("in {s}s"),
        s if s < 3_600 => format!("in {}m", s / 60),
        s if s < 86_400 => format!("in {}h", s / 3_600),
        s => format!("in {}d", s / 86_400),
    }
}

/// The task: `--prompt` (`-` reads stdin), or the words after `--`.
fn prompt(flag: Option<String>, words: &[String]) -> Result<Option<String>, CliError> {
    match (flag, words.is_empty()) {
        (Some(_), false) => Err(CliError::invalid(
            "give the task with --prompt or after --, not both",
        )),
        (Some(text), true) if text == "-" => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| CliError::invalid(format!("reading the task: {e}")))?;
            Ok(Some(text.trim_end().to_string()))
        }
        (Some(text), true) => Ok(Some(text)),
        (None, false) => Ok(Some(words.join(" "))),
        (None, true) => Ok(None),
    }
}

/// The schedule the flags name, if any.
fn schedule(args: &ScheduleArgs) -> Result<Option<Schedule>, CliError> {
    let schedule = if let Some(every) = &args.every {
        Schedule::every(every).ok_or_else(|| {
            CliError::invalid(format!("--every {every:?}: write it as 30m, 2h or 1d"))
        })?
    } else if let Some(at) = &args.daily {
        Schedule::Daily { at: at.clone() }
    } else if let (Some(days), Some(at)) = (&args.days, &args.at) {
        let days = Weekday::parse_list(days).ok_or_else(|| {
            CliError::invalid(format!(
                "--days {days:?}: name them as mon,wed,fri (or weekdays, weekends)"
            ))
        })?;
        Schedule::Weekdays {
            days,
            at: at.clone(),
        }
    } else {
        return Ok(None);
    };
    match schedule.problem() {
        Some(problem) => Err(CliError::invalid(problem)),
        None => Ok(Some(schedule)),
    }
}

/// A heartbeat by `hb_` id, else by name: this folder's, else the only one
/// of that name.
fn find(
    api: &dyn SessionApi,
    invocation: &Invocation,
    target: &str,
) -> Result<Heartbeat, CliError> {
    let heartbeats = api.heartbeats(None)?;
    let id = HeartbeatId::from_string(target);
    if let Some(found) = heartbeats.iter().find(|h| h.id == id) {
        return Ok(found.clone());
    }
    let named: Vec<&Heartbeat> = heartbeats.iter().filter(|h| h.name == target).collect();
    if let [only] = named.as_slice() {
        return Ok((*only).clone());
    }
    let here = here(api, invocation).ok();
    let mine: Vec<&&Heartbeat> = named
        .iter()
        .filter(|h| Some(&h.scope) == here.as_ref())
        .collect();
    match (named.len(), mine.as_slice()) {
        (0, _) => Err(CliError::Api(ApiError::new(
            ErrorCode::NotFound,
            format!("no heartbeat {target}"),
        ))),
        (_, [only]) => Ok((**only).clone()),
        _ => {
            let ids: Vec<String> = named.iter().map(|h| h.handle()).collect();
            Err(CliError::invalid(format!(
                "several heartbeats are named {target}: use an id ({})",
                ids.join(", ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_people_say_them() {
        assert_eq!(until(100, 100), "now");
        assert_eq!(until(0, 1_500), "in 25m");
        assert_eq!(until(0, 7_200), "in 2h");
        assert_eq!(until(0, 200_000), "in 2d");
        let every = ScheduleArgs {
            every: Some("90m".into()),
            ..Default::default()
        };
        assert_eq!(
            schedule(&every).unwrap(),
            Some(Schedule::Every { minutes: 90 })
        );
        let fast = ScheduleArgs {
            every: Some("2m".into()),
            ..Default::default()
        };
        assert!(schedule(&fast).is_err());
        let days = ScheduleArgs {
            days: Some("weekdays".into()),
            at: Some("09:30".into()),
            ..Default::default()
        };
        assert_eq!(
            schedule(&days).unwrap().map(|s| s.to_string()).as_deref(),
            Some("mon,tue,wed,thu,fri 09:30")
        );
        assert_eq!(
            prompt(None, &["update".into(), "deps".into()])
                .unwrap()
                .as_deref(),
            Some("update deps")
        );
        assert!(prompt(Some("a".into()), &["b".into()]).is_err());
    }
}
