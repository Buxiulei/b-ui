//! Hysteria2 直连/住宅配置渲染的形状校验（Task 7）。
mod common;

use bui_schema::{paths::Paths, render::hysteria};

#[test]
fn hysteria_direct_shape() {
    let s = common::state("obfs");
    let y: serde_yaml::Value =
        serde_yaml::from_str(&hysteria::direct_yaml(&s.node, &Paths::default_server())).unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":10000,20000-30000");
    assert_eq!(y["auth"]["type"].as_str().unwrap(), "command");
    assert_eq!(
        y["auth"]["command"].as_str().unwrap(),
        "/opt/b-ui/bin/bui-auth-hook"
    );
    assert_eq!(
        y["trafficStats"]["listen"].as_str().unwrap(),
        "127.0.0.1:9999"
    );
    assert_eq!(y["outbounds"][0]["direct"]["mode"].as_u64().unwrap(), 4);
    assert_eq!(
        y["tls"]["cert"].as_str().unwrap(),
        "/opt/b-ui/certs/fullchain.pem"
    );
    assert_eq!(y["obfs"]["type"].as_str().unwrap(), "salamander");
    assert_eq!(
        y["obfs"]["salamander"]["password"].as_str().unwrap(),
        "obfs-pw-test"
    );
    assert!(y["resolver"]["https"]["addr"]
        .as_str()
        .unwrap()
        .contains("1.1.1.1"));
    assert_eq!(y["masquerade"]["type"].as_str().unwrap(), "proxy");
}

#[test]
fn hysteria_direct_without_obfs_or_hop() {
    let mut s = common::state("global");
    s.node.ports.hy2_hop = None;
    let y: serde_yaml::Value =
        serde_yaml::from_str(&hysteria::direct_yaml(&s.node, &Paths::default_server())).unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":10000");
    assert!(y.get("obfs").is_none());
}

#[test]
fn hysteria_residential_shape() {
    let s = common::state("obfs");
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::residential_yaml(
        &s.node,
        &Paths::default_server(),
    ))
    .unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":40000,41000-50000");
    assert_eq!(
        y["trafficStats"]["listen"].as_str().unwrap(),
        "127.0.0.1:9998"
    );
    assert_eq!(y["outbounds"][0]["name"].as_str().unwrap(), "relay");
    assert_eq!(
        y["outbounds"][0]["socks5"]["addr"].as_str().unwrap(),
        "127.0.0.1:2080"
    );
    assert_eq!(y["acl"]["inline"][0].as_str().unwrap(), "relay(all)");
    assert!(y.get("obfs").is_none(), "v3 住宅实例不带 obfs，与订阅一致");
}

/// 事故回归（2026-09-12 bwg-rick，约 1 小时全员鉴权失败）：Hysteria2 的 `auth.command`
/// 只接受**单个可执行文件路径**——`extras/auth/command.go` 的
/// `CommandAuthenticator::Authenticate` 直接 `exec.Command(a.Cmd, addr, auth, tx)`，
/// 既不过 shell 也不按空格拆参数。渲染成 `/opt/b-ui/bin/bui auth-hook` 时内核找的是一个
/// **文件名里带空格**的可执行文件，每次鉴权都 exec 失败（H3：无法执行与拒绝在内核侧不可
/// 区分），两台实例全员 404。
///
/// 正式修法是让 `bin/bui-auth-hook`（指向同目录 `bui` 的符号链接）自己就是那个路径，
/// 由 `bui` 的 argv[0] 分发进钩子。两份配置都必须是**不含空白**的单路径。
#[test]
fn hysteria_auth_command_is_a_single_executable_path_without_arguments() {
    let s = common::state("obfs");
    let p = Paths::default_server();
    for (which, yaml) in [
        ("config.yaml", hysteria::direct_yaml(&s.node, &p)),
        (
            "config-residential.yaml",
            hysteria::residential_yaml(&s.node, &p),
        ),
    ] {
        let y: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let cmd = y["auth"]["command"].as_str().unwrap();
        assert_eq!(y["auth"]["type"].as_str().unwrap(), "command", "{which}");
        assert_eq!(
            cmd,
            p.auth_hook_bin().to_str().unwrap(),
            "{which} 的 auth.command 必须是 bin/bui-auth-hook"
        );
        assert_eq!(cmd, "/opt/b-ui/bin/bui-auth-hook", "{which}");
        assert!(
            !cmd.chars().any(char::is_whitespace),
            "{which} 的 auth.command 一旦带空白，内核就会去找一个文件名带空格的可执行文件：{cmd:?}"
        );
    }
}
