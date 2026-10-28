//! What Rakumux's processes use (`resource_usage`): the daemon, each
//! connected client's process and every live session's processes (the
//! program its terminal started and all that program's descendants), with
//! the machine's counters. Read from the process table when asked; nothing
//! is kept between requests, so any number of clients can ask, each turning
//! two samples into rates.

use crate::procinfo::{self, ProcStat};
use session_core::api::{
    ClientUsage, ProcessUsage, ResourceSample, SessionUsage, MAX_COMMAND_CHARS,
};
use session_core::ids::SessionId;
use std::collections::{HashMap, HashSet};

/// The most processes listed under one session (a parallel build can run
/// hundreds); the rest of its tree is left out.
const MAX_SESSION_PROCESSES: usize = 500;

/// A connection's client: the name its hello gave and the process on the
/// other end of the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProcess {
    pub client: String,
    pub pid: u32,
}

/// A live session and the program its terminal started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoot {
    pub id: SessionId,
    pub pid: u32,
}

/// Sample the machine now. `clients` are in connection order, one per
/// connection.
pub fn sample(daemon: u32, clients: &[ClientProcess], sessions: &[SessionRoot]) -> ResourceSample {
    let table = procinfo::process_table();
    let mut sample = assemble(&table, daemon, clients, sessions, &|pid| {
        let info = procinfo::process_info(pid)?;
        Some((info.name, command_line(&info.cmdline)))
    });
    sample.machine = procinfo::machine_usage();
    sample.at_ms = now_ms();
    add_traffic(&mut sample);
    sample
}

/// Name and command line of a process, or None once it's gone.
type Describe<'a> = dyn Fn(u32) -> Option<(String, String)> + 'a;

/// The sample's processes from `table`, without the machine's counters or
/// traffic. A session whose program isn't in the table (it just ended) is
/// left out, as is a client that runs inside a session (it's listed there)
/// or that has ended.
fn assemble(
    table: &[ProcStat],
    daemon: u32,
    clients: &[ClientProcess],
    sessions: &[SessionRoot],
    describe: &Describe,
) -> ResourceSample {
    let by_pid: HashMap<u32, &ProcStat> = table.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in table {
        if process.ppid != process.pid {
            children.entry(process.ppid).or_default().push(process.pid);
        }
    }
    for list in children.values_mut() {
        list.sort_unstable();
    }
    let usage = |pid: u32, parent: Option<u32>| -> Option<ProcessUsage> {
        let stat = by_pid.get(&pid)?;
        let (name, command) = describe(pid)?;
        Some(ProcessUsage {
            pid,
            parent,
            name,
            command,
            cpu_ms: stat.cpu_ms,
            rss_bytes: stat.rss_bytes,
            net_rx_bytes: None,
            net_tx_bytes: None,
        })
    };

    let mut in_sessions = HashSet::new();
    let mut session_usage = Vec::with_capacity(sessions.len());
    for session in sessions {
        let Some(root) = usage(session.pid, None) else {
            continue;
        };
        in_sessions.insert(root.pid);
        let mut processes = vec![root];
        // Depth first, each process's children in pid order.
        let mut stack: Vec<(u32, u32)> = children
            .get(&session.pid)
            .map(|kids| kids.iter().rev().map(|kid| (*kid, session.pid)).collect())
            .unwrap_or_default();
        while let Some((pid, parent)) = stack.pop() {
            if processes.len() >= MAX_SESSION_PROCESSES || !in_sessions.insert(pid) {
                continue;
            }
            let Some(process) = usage(pid, Some(parent)) else {
                continue;
            };
            processes.push(process);
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids.iter().rev().map(|kid| (*kid, pid)));
            }
        }
        session_usage.push(SessionUsage {
            id: session.id.clone(),
            processes,
        });
    }

    let mut client_usage: Vec<ClientUsage> = Vec::new();
    for client in clients {
        if client.pid == daemon || in_sessions.contains(&client.pid) {
            continue;
        }
        if let Some(listed) = client_usage
            .iter_mut()
            .find(|c| c.process.pid == client.pid)
        {
            listed.connections += 1;
            continue;
        }
        if let Some(process) = usage(client.pid, None) {
            client_usage.push(ClientUsage {
                client: client.client.clone(),
                connections: 1,
                process,
            });
        }
    }

    let daemon_usage = usage(daemon, None).unwrap_or_else(|| ProcessUsage {
        pid: daemon,
        parent: None,
        name: "rakumux-ptyd".to_string(),
        command: String::new(),
        cpu_ms: 0,
        rss_bytes: procinfo::self_rss_bytes().unwrap_or(0),
        net_rx_bytes: None,
        net_tx_bytes: None,
    });
    ResourceSample {
        at_ms: 0,
        machine: Default::default(),
        daemon: daemon_usage,
        clients: client_usage,
        sessions: session_usage,
    }
}

/// Every listed process's TCP traffic, where the platform says; a
/// connection two processes share counts for the first listed.
fn add_traffic(sample: &mut ResourceSample) {
    let mut processes: Vec<&mut ProcessUsage> = std::iter::once(&mut sample.daemon)
        .chain(sample.clients.iter_mut().map(|c| &mut c.process))
        .chain(
            sample
                .sessions
                .iter_mut()
                .flat_map(|s| s.processes.iter_mut()),
        )
        .collect();
    let pids: Vec<u32> = processes.iter().map(|p| p.pid).collect();
    let Some(traffic) = procinfo::tcp_traffic(&pids) else {
        return;
    };
    for process in processes.iter_mut() {
        if let Some((rx, tx)) = traffic.get(&process.pid) {
            process.net_rx_bytes = Some(*rx);
            process.net_tx_bytes = Some(*tx);
        }
    }
}

/// Arguments joined by spaces, at most [`MAX_COMMAND_CHARS`] characters.
fn command_line(cmdline: &[String]) -> String {
    cmdline.join(" ").chars().take(MAX_COMMAND_CHARS).collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(pid: u32, ppid: u32, cpu_ms: u64, rss_bytes: u64) -> ProcStat {
        ProcStat {
            pid,
            ppid,
            cpu_ms,
            rss_bytes,
        }
    }

    fn describe(pid: u32) -> Option<(String, String)> {
        let name = match pid {
            1 => "init",
            10 => "rakumux-ptyd",
            20 => "tui",
            30 => "bash",
            31 => "claude",
            32 => "node",
            33 => "rg",
            40 => "zsh",
            50 => "rakumux",
            _ => return None,
        };
        Some((name.to_string(), format!("{name} --flag")))
    }

    fn client(client: &str, pid: u32) -> ClientProcess {
        ClientProcess {
            client: client.to_string(),
            pid,
        }
    }

    fn table() -> Vec<ProcStat> {
        vec![
            stat(1, 0, 5, 1),
            stat(10, 1, 100, 30),
            stat(20, 1, 200, 40),
            stat(30, 10, 1, 5),
            stat(31, 30, 9000, 800),
            stat(33, 31, 10, 2),
            stat(32, 31, 50, 25),
            stat(40, 10, 2, 6),
            // Runs inside the first session: listed there, not as a client.
            stat(50, 32, 1, 1),
        ]
    }

    #[test]
    fn sessions_list_their_whole_tree_depth_first() {
        let (a, b) = (SessionId::generate(), SessionId::generate());
        let sessions = [
            SessionRoot {
                id: a.clone(),
                pid: 30,
            },
            SessionRoot {
                id: b.clone(),
                pid: 40,
            },
            // Its program just ended.
            SessionRoot {
                id: SessionId::generate(),
                pid: 99,
            },
        ];
        let sample = assemble(&table(), 10, &[], &sessions, &describe);
        assert_eq!(sample.sessions.len(), 2);
        let first: Vec<(u32, Option<u32>)> = sample.sessions[0]
            .processes
            .iter()
            .map(|p| (p.pid, p.parent))
            .collect();
        assert_eq!(
            first,
            [
                (30, None),
                (31, Some(30)),
                (32, Some(31)),
                (50, Some(32)),
                (33, Some(31))
            ]
        );
        assert_eq!(sample.sessions[0].id, a);
        assert_eq!(sample.sessions[1].id, b);
        assert_eq!(sample.sessions[1].processes.len(), 1);
        let claude = &sample.sessions[0].processes[1];
        assert_eq!(
            (
                claude.name.as_str(),
                claude.command.as_str(),
                claude.cpu_ms,
                claude.rss_bytes
            ),
            ("claude", "claude --flag", 9000, 800)
        );
        assert_eq!(sample.daemon.pid, 10);
        assert_eq!(sample.daemon.rss_bytes, 30);
    }

    #[test]
    fn clients_are_listed_once_per_process_outside_sessions() {
        let sessions = [SessionRoot {
            id: SessionId::generate(),
            pid: 30,
        }];
        let clients = [
            client("tui", 20),
            client("tui", 20),
            client("cli", 50),
            client("api", 77),
            client("tui", 20),
        ];
        let sample = assemble(&table(), 10, &clients, &sessions, &describe);
        assert_eq!(sample.clients.len(), 1);
        assert_eq!(sample.clients[0].client, "tui");
        assert_eq!(sample.clients[0].connections, 3);
        assert_eq!(sample.clients[0].process.cpu_ms, 200);
    }

    #[test]
    fn a_parent_cycle_or_a_huge_tree_stays_bounded() {
        let mut table = vec![stat(30, 31, 0, 0), stat(31, 30, 0, 0)];
        table.extend((1000..2000).map(|pid| stat(pid, 30, 0, 0)));
        let sessions = [SessionRoot {
            id: SessionId::generate(),
            pid: 30,
        }];
        let sample = assemble(&table, 10, &[], &sessions, &|pid| {
            Some((pid.to_string(), String::new()))
        });
        let processes = &sample.sessions[0].processes;
        assert_eq!(processes.len(), MAX_SESSION_PROCESSES);
        let unique: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
        assert_eq!(unique.len(), processes.len());
    }

    #[test]
    fn command_lines_are_cut() {
        let long = vec!["x".repeat(MAX_COMMAND_CHARS * 2)];
        assert_eq!(command_line(&long).chars().count(), MAX_COMMAND_CHARS);
        assert_eq!(command_line(&["a".into(), "b c".into()]), "a b c");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_live_sample_finds_this_process_and_its_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let me = std::process::id();
        let sessions = [SessionRoot {
            id: SessionId::generate(),
            pid: me,
        }];
        let mut sample = sample(me, &[client("tui", me)], &sessions);
        for _ in 0..100 {
            let found = sample.sessions[0]
                .processes
                .iter()
                .any(|p| p.pid == child.id() && p.name == "sleep");
            if found {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            sample = super::sample(me, &[], &sessions);
        }
        let _ = child.kill();
        let _ = child.wait();
        let processes = &sample.sessions[0].processes;
        assert!(
            processes
                .iter()
                .any(|p| p.pid == child.id() && p.parent == Some(me)),
            "{processes:?}"
        );
        assert!(processes[0].rss_bytes > 0);
        assert!(processes[0].net_rx_bytes.is_some());
        assert!(sample.machine.cpus >= 1);
        assert!(sample.machine.memory_total_bytes.unwrap_or(0) > 0);
        assert!(sample.machine.cpu_total_ms.unwrap_or(0) > 0);
        assert!(sample.at_ms > 0);
    }
}
