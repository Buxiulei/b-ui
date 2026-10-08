//! 恢复授权必须属于仍然存在的实例，并在动作锁内重新验证。
use bui_c::check::{self, Runtime};
use bui_c::fake::{FakeNet, FakeReply, FakeSys};
use bui_c::paths::Paths;
use bui_c::profiles::Profiles;
use bui_c::sys::Sys;
use bui_c::testutil::profiles_tun;

const SHOW: &str = "systemctl show --no-pager --property=LoadState,ActiveState,SubState,InvocationID,MainPID,ExecMainStartTimestampMonotonic bui-c.service";
const ROUTES: &str = "ip -4 route show table all";

fn running(id: u128) -> String {
    format!("LoadState=loaded\nActiveState=active\nSubState=running\nInvocationID={id:032x}\nMainPID=101\nExecMainStartTimestampMonotonic=1001\n")
}

fn fixture() -> (FakeSys, FakeNet, Paths) {
    let s = FakeSys::new();
    let n = FakeNet::new();
    let paths = Paths::new("/opt/bui-c", "/etc/systemd/system");
    let mut p = profiles_tun();
    p.auto_update = false;
    p.save(&s, &paths).unwrap();
    s.put("/opt/bui-c/config.json", "{}");
    s.put("/opt/bui-c/bin/sing-box", "fixture");
    s.reply("systemctl is-active --quiet bui-c.service", 0, "");
    s.reply(SHOW, 0, &running(1));
    s.reply("ip link show bui-tun", 0, "5: bui-tun");
    bui_c::testutil::present_tun(&s);
    s.reply(ROUTES, 0, "");
    n.route(check::PROBE_URL, FakeReply::Status(204));
    (s, n, paths)
}

fn assert_no_restart(s: &FakeSys) {
    assert!(
        !s.called("systemctl restart bui-c.service"),
        "{:?}",
        s.calls()
    );
}

#[test]
fn recovery_owner_discards_a_failure_after_the_same_profile_gets_a_new_instance() {
    let (s, n, paths) = fixture();
    s.stage_reply_on_lock(SHOW, 0, &running(2));
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert!(s.called("lock"), "必须实际到达动作锁");
    assert_eq!(Runtime::load(&s, &paths).last_restart_at, None);
}

#[test]
fn recovery_owner_rechecks_the_local_failure_inside_the_lock() {
    let (s, n, paths) = fixture();
    s.stage_reply_on_lock(ROUTES, 0, "default dev bui-tun table 2022\n");
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    let calls = s.calls();
    let lock = calls.iter().position(|c| c == "lock").unwrap();
    assert!(calls.iter().skip(lock + 1).any(|c| c == ROUTES));
}

#[test]
fn recovery_owner_discards_a_failure_when_config_bytes_change_without_profile_changes() {
    let (s, n, paths) = fixture();
    s.stage_on_lock("/opt/bui-c/config.json", "{\"changed\":true}");
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert!(s.called("lock"));
}

#[test]
fn recovery_owner_discards_a_failure_when_credentials_change_under_the_same_name() {
    let (s, n, paths) = fixture();
    let mut p = Profiles::load(&s, &paths).unwrap();
    if let bui_schema::nodes::Transport::Hysteria2 { password, .. } =
        &mut p.profiles[0].node.transport
    {
        *password = "replacement-fixture-password".into();
    } else {
        panic!("fixture必须实际是HY2节点");
    }
    s.stage_on_lock(
        "/opt/bui-c/profiles.json",
        &serde_json::to_string(&p).unwrap(),
    );
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert!(s.called("lock"));
}

#[test]
fn recovery_owner_uses_the_restart_backoff_written_while_waiting_for_the_lock() {
    let (s, n, paths) = fixture();
    let newer = Runtime {
        fail_streak: 3,
        last_restart_at: Some(s.now().unix_timestamp()),
        ..Runtime::default()
    };
    s.stage_on_lock(
        "/opt/bui-c/runtime.json",
        &serde_json::to_string(&newer).unwrap(),
    );
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert_eq!(Runtime::load(&s, &paths), newer);
}

#[test]
fn recovery_owner_clears_streak_without_overwriting_concurrent_update_fields() {
    let (s, n, paths) = fixture();
    s.reply(ROUTES, 0, "default dev bui-tun table 2022\n");
    let original = Runtime {
        fail_streak: 2,
        ..Runtime::default()
    };
    original.save(&s, &paths).unwrap();
    let newer = Runtime {
        last_update_at: Some(77),
        update_available: true,
        ..original
    };
    s.stage_on_lock(
        "/opt/bui-c/runtime.json",
        &serde_json::to_string(&newer).unwrap(),
    );
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert!(s.called("lock"), "健康分支修改记账也必须持动作锁");
    let after = Runtime::load(&s, &paths);
    assert_eq!(after.fail_streak, 0);
    assert_eq!(after.last_update_at, Some(77));
    assert!(after.update_available);
}

#[test]
fn recovery_owner_does_not_turn_an_unreadable_instance_into_restart_authorization() {
    let (s, n, paths) = fixture();
    s.reply(SHOW, 1, "");
    assert!(matches!(
        check::run(&s, &n, &paths).unwrap(),
        check::Verdict::Uncertain { .. }
    ));
    assert_no_restart(&s);
    assert!(s.called(SHOW), "必须实际尝试观察systemd实例");
}

#[test]
fn recovery_owner_keeps_query_failures_unknown_instead_of_missing_capture() {
    for query in ["ip -j link show", ROUTES] {
        let (s, n, paths) = fixture();
        s.reply(query, 1, "");
        assert!(matches!(
            check::run(&s, &n, &paths).unwrap(),
            check::Verdict::Uncertain { .. }
        ));
        assert_no_restart(&s);
        assert!(s.called(query));
        assert!(n.log().is_empty(), "本地观察不确定时必须跳过外部探测");
    }
}

#[test]
fn recovery_owner_still_recovers_a_confirmed_local_fault_after_revalidation() {
    let (s, n, paths) = fixture();
    let result = check::run(&s, &n, &paths).unwrap();
    assert!(
        matches!(result, check::Verdict::Restarted { .. }),
        "{result:?}"
    );
    let calls = s.calls();
    let lock = calls.iter().position(|c| c == "lock").unwrap();
    let restart = calls
        .iter()
        .position(|c| c == "systemctl restart bui-c.service")
        .unwrap();
    assert!(calls[lock + 1..restart].iter().any(|c| c == SHOW));
    assert!(calls[lock + 1..restart].iter().any(|c| c == ROUTES));
    assert_eq!(Runtime::load(&s, &paths).fail_streak, 1);
    assert!(n.log().is_empty(), "本地捕获缺失时不发送外部探测");
}

#[test]
fn recovery_owner_fences_instance_changes_during_locked_local_queries() {
    let (s, n, paths) = fixture();
    let original = running(1);
    let replacement = running(2);
    s.reply_sequence(
        SHOW,
        &[
            (0, &original),
            (0, &original),
            (0, &original),
            (0, &replacement),
        ],
    );
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert!(
        s.calls().iter().filter(|c| *c == SHOW).count() >= 4,
        "动作前须在本地查询后重新核实例"
    );
}

#[test]
fn recovery_owner_fences_streak_clear_when_the_instance_changes_during_local_queries() {
    let (s, n, paths) = fixture();
    s.reply(ROUTES, 0, "default dev bui-tun table 2022\n");
    let rt = Runtime {
        fail_streak: 2,
        ..Runtime::default()
    };
    rt.save(&s, &paths).unwrap();
    let original = running(1);
    let replacement = running(2);
    s.reply_sequence(
        SHOW,
        &[
            (0, &original),
            (0, &original),
            (0, &original),
            (0, &replacement),
        ],
    );
    check::run(&s, &n, &paths).unwrap();
    assert_no_restart(&s);
    assert_eq!(
        Runtime::load(&s, &paths),
        rt,
        "旧实例成功探测不能清除新代次恢复记录"
    );
    assert!(s.calls().iter().filter(|c| *c == SHOW).count() >= 4);
}

#[test]
fn recovery_owner_does_not_probe_a_physical_path_without_confirmed_capture() {
    for reply in [(1, ""), (0, ""), (0, "default dev eth0\n")] {
        let (s, n, paths) = fixture();
        s.reply(ROUTES, reply.0, reply.1);
        check::run(&s, &n, &paths).unwrap();
        assert!(
            n.log().is_empty(),
            "捕获未确认或未接管时，不得经Via::Direct发送外部探测：{:?}",
            n.log()
        );
    }
}
