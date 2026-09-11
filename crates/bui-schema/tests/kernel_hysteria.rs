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
        "/opt/b-ui/bin/bui auth-hook"
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
