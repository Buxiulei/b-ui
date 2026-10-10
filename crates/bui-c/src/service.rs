//! 单次 systemd 快照：查询不确定时不授权恢复动作。

use crate::sys::Sys;
use std::collections::BTreeMap;

/// 正在运行的一次服务实例。PID 相同也可能已经经历了一次重启。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub invocation_id: String,
    pub main_pid: u32,
    pub exec_main_start_timestamp_monotonic: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedKind {
    Inactive,
    Failed,
}

/// 明确停下的服务以及最后可见的实例代次；未启动过时没有 invocation。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedState {
    pub active_state: StoppedKind,
    pub invocation_id: Option<String>,
    pub exec_main_start_timestamp_monotonic: u64,
}

/// 只有完整的运行实例或明确的停止状态可以参与恢复决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceObservation {
    Running(Instance),
    Stopped(StoppedState),
    Transitioning,
    Unknown,
}

/// 一次 show 获取同一个快照，避免逐字段查询把不同服务实例拼到一起。
/// 执行失败或输出不能完整解释时返回 Unknown，不输出原始命令载荷。
pub fn observe(sys: &impl Sys, unit: &str) -> ServiceObservation {
    let Ok(output) = sys.run(
        "systemctl",
        &[
            "show",
            "--no-pager",
            "--property=LoadState,ActiveState,SubState,InvocationID,MainPID,ExecMainStartTimestampMonotonic",
            unit,
        ],
    ) else {
        return ServiceObservation::Unknown;
    };
    if !output.ok() {
        return ServiceObservation::Unknown;
    }
    parse(&output.stdout).unwrap_or(ServiceObservation::Unknown)
}

fn parse(stdout: &str) -> Option<ServiceObservation> {
    const FIELDS: [&str; 6] = [
        "LoadState",
        "ActiveState",
        "SubState",
        "InvocationID",
        "MainPID",
        "ExecMainStartTimestampMonotonic",
    ];
    let mut properties = BTreeMap::new();
    for line in stdout.lines() {
        let (key, value) = line.split_once('=')?;
        if !FIELDS.contains(&key) || properties.insert(key, value).is_some() {
            return None;
        }
    }
    if properties.len() != FIELDS.len() || properties.get("LoadState") != Some(&"loaded") {
        return None;
    }

    let active = *properties.get("ActiveState")?;
    let sub = *properties.get("SubState")?;
    if active.is_empty() || sub.is_empty() {
        return None;
    }
    let pid = *properties.get("MainPID")?;
    let start = *properties.get("ExecMainStartTimestampMonotonic")?;
    if [pid, start]
        .iter()
        .any(|value| value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let main_pid = pid.parse::<u32>().ok()?;
    let exec_main_start_timestamp_monotonic = start.parse::<u64>().ok()?;
    let raw_id = *properties.get("InvocationID")?;
    let invocation_id = if raw_id.is_empty() {
        None
    } else {
        if raw_id.len() != 32 || !raw_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        (!raw_id.bytes().all(|byte| byte == b'0')).then(|| raw_id.to_ascii_lowercase())
    };

    match (active, sub, main_pid) {
        ("active", "running", pid) if pid != 0 && exec_main_start_timestamp_monotonic != 0 => {
            Some(ServiceObservation::Running(Instance {
                invocation_id: invocation_id?,
                main_pid,
                exec_main_start_timestamp_monotonic,
            }))
        }
        ("inactive", "dead", 0) | ("failed", "failed", 0) => {
            Some(ServiceObservation::Stopped(StoppedState {
                active_state: if active == "inactive" {
                    StoppedKind::Inactive
                } else {
                    StoppedKind::Failed
                },
                invocation_id,
                exec_main_start_timestamp_monotonic,
            }))
        }
        ("activating" | "deactivating" | "reloading", _, _) => {
            Some(ServiceObservation::Transitioning)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;

    const SHOW: &str = "systemctl show --no-pager --property=LoadState,ActiveState,SubState,InvocationID,MainPID,ExecMainStartTimestampMonotonic bui-c.service";

    fn invocation(seed: u128) -> String {
        format!("{seed:032x}")
    }

    fn snapshot(active: &str, sub: &str, id: &str, pid: u32, start: u64) -> String {
        format!(
            "LoadState=loaded\nActiveState={active}\nSubState={sub}\nInvocationID={id}\nMainPID={pid}\nExecMainStartTimestampMonotonic={start}\n"
        )
    }

    fn from_output(code: i32, output: &str) -> ServiceObservation {
        let sys = FakeSys::new();
        sys.reply(SHOW, code, output);
        let observed = observe(&sys, "bui-c.service");
        assert!(sys.called(SHOW), "必须查询真实约定的完整 systemd 快照");
        observed
    }

    #[test]
    fn loaded_running_service_has_an_observable_instance() {
        let id = invocation(1);
        let observed = from_output(0, &snapshot("active", "running", &id, 101, 1001));
        assert_eq!(
            observed,
            ServiceObservation::Running(Instance {
                invocation_id: id,
                main_pid: 101,
                exec_main_start_timestamp_monotonic: 1001,
            })
        );
    }

    #[test]
    fn property_order_does_not_change_the_instance() {
        let id = invocation(2);
        let reordered = format!(
            "MainPID=102\nSubState=running\nLoadState=loaded\nExecMainStartTimestampMonotonic=1002\nInvocationID={id}\nActiveState=active\n"
        );
        assert_eq!(
            from_output(0, &reordered),
            ServiceObservation::Running(Instance {
                invocation_id: id,
                main_pid: 102,
                exec_main_start_timestamp_monotonic: 1002,
            })
        );
    }

    #[test]
    fn different_invocations_are_different_instances_even_with_the_same_pid() {
        let first = from_output(0, &snapshot("active", "running", &invocation(3), 103, 1003));
        let second = from_output(0, &snapshot("active", "running", &invocation(4), 103, 1003));
        assert!(matches!(first, ServiceObservation::Running(_)));
        assert!(matches!(second, ServiceObservation::Running(_)));
        assert_ne!(first, second);
    }

    #[test]
    fn pid_and_start_timestamp_are_part_of_instance_identity() {
        let id = invocation(5);
        let first = from_output(0, &snapshot("active", "running", &id, 105, 1005));
        let other_pid = from_output(0, &snapshot("active", "running", &id, 106, 1005));
        let other_start = from_output(0, &snapshot("active", "running", &id, 105, 1006));
        assert!(matches!(first, ServiceObservation::Running(_)));
        assert_ne!(first, other_pid);
        assert_ne!(first, other_start);
    }

    #[test]
    fn failed_query_does_not_trust_a_running_payload() {
        assert_eq!(
            from_output(1, &snapshot("active", "running", &invocation(6), 106, 1006)),
            ServiceObservation::Unknown
        );
    }

    #[test]
    fn blank_or_incomplete_output_is_unknown() {
        for output in ["", "\n", "LoadState=loaded\nActiveState=active\n"] {
            assert_eq!(from_output(0, output), ServiceObservation::Unknown);
        }
        let valid = snapshot("active", "running", &invocation(7), 107, 1007);
        for field in [
            "LoadState",
            "ActiveState",
            "SubState",
            "InvocationID",
            "MainPID",
            "ExecMainStartTimestampMonotonic",
        ] {
            let missing = valid
                .lines()
                .filter(|line| !line.starts_with(&format!("{field}=")))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                from_output(0, &missing),
                ServiceObservation::Unknown,
                "missing {field}"
            );
        }
    }

    #[test]
    fn malformed_values_or_duplicate_properties_are_unknown() {
        let id = invocation(8);
        let valid = snapshot("active", "running", &id, 108, 1008);
        for (before, after) in [
            ("LoadState=loaded", "LoadState=not-found"),
            ("ActiveState=active", "ActiveState="),
            ("SubState=running", "SubState="),
            ("MainPID=108", "MainPID=abc"),
            ("MainPID=108", "MainPID=-1"),
            ("MainPID=108", "MainPID=4294967296"),
            (
                "ExecMainStartTimestampMonotonic=1008",
                "ExecMainStartTimestampMonotonic=",
            ),
            (
                "ExecMainStartTimestampMonotonic=1008",
                "ExecMainStartTimestampMonotonic=-1",
            ),
            (
                "ExecMainStartTimestampMonotonic=1008",
                "ExecMainStartTimestampMonotonic=18446744073709551616",
            ),
        ] {
            assert_eq!(
                from_output(0, &valid.replace(before, after)),
                ServiceObservation::Unknown,
                "{after}"
            );
        }
        for malformed_id in ["", "z", "not-a-systemd-invocation", "0000"] {
            assert_eq!(
                from_output(0, &snapshot("active", "running", malformed_id, 108, 1008)),
                ServiceObservation::Unknown
            );
        }
        for line in valid.lines() {
            let duplicate = format!("{valid}{line}\n");
            assert_eq!(from_output(0, &duplicate), ServiceObservation::Unknown);
        }
        assert_eq!(
            from_output(0, &format!("{valid}not-a-property\n")),
            ServiceObservation::Unknown
        );
    }

    #[test]
    fn active_without_a_live_pid_and_complete_epoch_is_unknown() {
        for (id, pid, start) in [
            (invocation(9), 0, 1009),
            (invocation(0), 109, 1009),
            (invocation(9), 109, 0),
        ] {
            assert_eq!(
                from_output(0, &snapshot("active", "running", &id, pid, start)),
                ServiceObservation::Unknown
            );
        }
    }

    #[test]
    fn inactive_and_failed_services_preserve_their_stopped_epoch() {
        let id = invocation(10);
        for (active, sub, expected) in [
            ("inactive", "dead", StoppedKind::Inactive),
            ("failed", "failed", StoppedKind::Failed),
        ] {
            assert_eq!(
                from_output(0, &snapshot(active, sub, &id, 0, 1010)),
                ServiceObservation::Stopped(StoppedState {
                    active_state: expected,
                    invocation_id: Some(id.clone()),
                    exec_main_start_timestamp_monotonic: 1010,
                })
            );
        }
    }

    #[test]
    fn never_started_inactive_service_is_explicitly_stopped() {
        for id in [String::new(), invocation(0)] {
            assert_eq!(
                from_output(0, &snapshot("inactive", "dead", &id, 0, 0)),
                ServiceObservation::Stopped(StoppedState {
                    active_state: StoppedKind::Inactive,
                    invocation_id: None,
                    exec_main_start_timestamp_monotonic: 0,
                })
            );
        }
    }

    #[test]
    fn contradictory_states_are_unknown() {
        for (active, sub, pid) in [
            ("inactive", "dead", 111),
            ("failed", "failed", 111),
            ("inactive", "running", 0),
            ("failed", "dead", 0),
            ("active", "dead", 111),
            ("active", "exited", 111),
            ("maintenance", "unknown", 0),
        ] {
            assert_eq!(
                from_output(0, &snapshot(active, sub, &invocation(11), pid, 1011)),
                ServiceObservation::Unknown,
                "{active}/{sub}/{pid}"
            );
        }
    }

    #[test]
    fn start_stop_and_reload_transitions_are_not_stopped_or_running() {
        for (active, sub, pid) in [
            ("activating", "start-pre", 0),
            ("deactivating", "stop-sigterm", 112),
            ("reloading", "reload", 112),
        ] {
            assert_eq!(
                from_output(0, &snapshot(active, sub, &invocation(12), pid, 1012)),
                ServiceObservation::Transitioning
            );
        }
    }
}
