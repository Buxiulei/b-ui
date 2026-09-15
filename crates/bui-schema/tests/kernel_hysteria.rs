//! Hysteria2 直连/住宅配置渲染的形状校验（Task 7）+ 真实内核的配置加载校验。
mod common;

use bui_schema::model::Hy2Auth;
use bui_schema::{paths::Paths, render::hysteria};

/// 随便要一个空闲 UDP 端口（真起内核的那条用例要一个没人占的 `listen:`）。
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// 自签一张证书（`hysteria` 在校验 `auth` 之前先 `stat` 证书，缺了就走不到 auth 那一步）。
/// 没有 `openssl` 就返回 `None`，调用方跳过。
fn self_signed(dir: &std::path::Path) -> Option<()> {
    let out = std::process::Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .arg("-keyout")
        .arg(dir.join("privkey.pem"))
        .arg("-out")
        .arg(dir.join("fullchain.pem"))
        .args(["-subj", "/CN=test.invalid"])
        .output()
        .ok()?;
    out.status.success().then_some(())
}

/// golden：`tests/fixtures/hysteria/<模式>/<文件名>`，两种鉴权模式各一份。
fn golden(mode: &str, file: &str) -> String {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/hysteria"
    ))
    .join(mode)
    .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 {}：{e}", path.display()))
}

#[test]
fn hysteria_direct_shape() {
    let s = common::state("obfs");
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::direct_yaml(
        &s.node,
        &Paths::default_server(),
        Hy2Auth::Command,
    ))
    .unwrap();
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
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::direct_yaml(
        &s.node,
        &Paths::default_server(),
        Hy2Auth::Http,
    ))
    .unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":10000");
    assert!(y.get("obfs").is_none());
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::residential_yaml(
        &s.node,
        &Paths::default_server(),
        Hy2Auth::Http,
    ))
    .unwrap();
    assert!(y.get("obfs").is_none(), "混淆关闭时住宅实例也不带 obfs");
}

#[test]
fn hysteria_residential_shape() {
    let s = common::state("obfs");
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::residential_yaml(
        &s.node,
        &Paths::default_server(),
        Hy2Auth::Command,
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
    // 混淆覆盖全部 HY2 实例（2026-09-15 裁决），与订阅里住宅 HY2 节点的 obfs 参数一致
    assert_eq!(y["obfs"]["type"].as_str().unwrap(), "salamander");
    assert_eq!(
        y["obfs"]["salamander"]["password"].as_str().unwrap(),
        "obfs-pw-test"
    );
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
        (
            "config.yaml",
            hysteria::direct_yaml(&s.node, &p, Hy2Auth::Command),
        ),
        (
            "config-residential.yaml",
            hysteria::residential_yaml(&s.node, &p, Hy2Auth::Command),
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

/// 三个住宅实例的配置都得是合法 YAML、`listen` 行互不重叠 —— hysteria 没有
/// `check` 子命令（spec §2.2），所以这一层只能靠 schema 测试兜底。
#[test]
fn three_residential_slot_configs_parse_and_do_not_overlap() {
    let s = common::state("global");
    let p = bui_schema::paths::Paths::default_server();
    let mut seen: Vec<(u16, u16)> = vec![];
    for i in 0..3u16 {
        let r = bui_schema::slots::resources(&s.node.ports, i, 3);
        let text =
            bui_schema::render::hysteria::residential_slot_yaml(&s.node, &p, &r, Hy2Auth::Http);
        let v: serde_yaml::Value = serde_yaml::from_str(&text).expect("合法 YAML");
        assert_eq!(
            v["listen"].as_str().unwrap(),
            format!(":{},{}-{}", r.hy2_port, r.hop.0, r.hop.1)
        );
        assert_eq!(
            v["outbounds"][0]["socks5"]["addr"].as_str().unwrap(),
            format!("127.0.0.1:{}", r.relay_port)
        );
        assert_eq!(
            v["trafficStats"]["listen"].as_str().unwrap(),
            format!("127.0.0.1:{}", r.stats_port)
        );
        for (lo, hi) in &seen {
            assert!(
                r.hop.1 < *lo || r.hop.0 > *hi,
                "槽 {i} 的跳跃区间与已有区间重叠"
            );
        }
        seen.push(r.hop);
    }
}

/// 真实内核校验：hysteria **没有** `check` 子命令（spec §2.2），所以只能真起一次
/// `hysteria server -c`，看它在**配置加载阶段**报不报错。
///
/// 这一层不是摆设：`auth.type` 是内核在加载期校验的（写错就是
/// `invalid config: auth.type: unsupported auth type`，实测 v2.12.2），而 `auth.http`
/// 这个形状 YAML 层面怎么写都合法 —— 2026-09-12 那次全员鉴权失败正是这类「静态校验看不出」
/// 的错配。加载走到 auth 之前要先过 `listen` 与 `tls.cert`，所以这里去掉端口跳跃
/// （nft 建表要 root）并临时自签一张证书。
///
/// 内核不在 PATH 上、或本机没有 openssl 就跳过（与 `sing-box` / `xray` 那两条同口径）。
#[test]
fn both_auth_modes_load_in_the_real_hysteria_kernel() {
    if !common::have("hysteria") {
        eprintln!("skipped: hysteria not found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    if self_signed(dir.path()).is_none() {
        eprintln!("skipped: openssl not usable");
        return;
    }
    let mut s = common::state("obfs");
    s.node.ports.hy2_hop = None; // 端口跳跃要 root 建 nft 表
    s.node.ports.hy2 = free_udp_port();
    let paths = Paths {
        base_dir: dir.path().to_path_buf(),
        certs_dir: dir.path().to_path_buf(),
        bin_dir: dir.path().join("bin"),
    };
    let slot0 = bui_schema::slots::resources(&s.node.ports, 0, 1);
    for (mode, auth) in [("http", Hy2Auth::Http), ("command", Hy2Auth::Command)] {
        for (which, text) in [
            ("direct", hysteria::direct_yaml(&s.node, &paths, auth)),
            // 住宅实例同样带 obfs（2026-09-15 裁决）；跳跃区间同样要 root，换成单端口
            (
                "residential",
                with_listen(
                    &hysteria::residential_slot_yaml(&s.node, &paths, &slot0, auth),
                    &format!(":{}", free_udp_port()),
                ),
            ),
        ] {
            let mode = format!("{mode}-{which}");
            assert!(text.contains("salamander"), "{mode} 应带 obfs：{text}");
            let cfg = dir.path().join(format!("{mode}.yaml"));
            std::fs::write(&cfg, text).unwrap();
            let log = std::fs::File::create(dir.path().join(format!("{mode}.log"))).unwrap();
            let mut child = std::process::Command::new("hysteria")
                .args(["server", "--disable-update-check", "-c"])
                .arg(&cfg)
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap();
            // 加载失败是**立刻**退出；成功就一直跑，等 2 秒足够分辨这两种
            for _ in 0..40 {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
            let out = std::fs::read_to_string(dir.path().join(format!("{mode}.log"))).unwrap();
            assert!(
                !out.contains("invalid config"),
                "{mode} 模式的配置内核不认：{out}"
            );
        }
    }
}

/// 把渲染出的 hysteria 配置里的一个顶层键换掉（真起内核时去掉要 root 或会撞端口的部分）。
fn with_key(yaml: &str, key: &str, value: serde_yaml::Value) -> String {
    let mut doc: serde_yaml::Mapping = serde_yaml::from_str(yaml).unwrap();
    doc.insert(serde_yaml::Value::from(key), value);
    serde_yaml::to_string(&doc).unwrap()
}

fn with_listen(yaml: &str, listen: &str) -> String {
    with_key(yaml, "listen", serde_yaml::Value::from(listen))
}

/// 随便要一个空闲 TCP 端口（客户端的 socks5 入站、服务端的 trafficStats）。
fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// 在 `deadline` 之内等 `path` 里出现 `needle`；子进程提前退出也算等完。
fn wait_for_log(
    path: &std::path::Path,
    needle: &str,
    child: &mut std::process::Child,
    deadline: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains(needle)
        {
            return true;
        }
        if child.try_wait().unwrap().is_some() {
            return std::fs::read_to_string(path)
                .unwrap_or_default()
                .contains(needle);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// 端到端（本机回环，不出网）：obfs 开启时渲染出的直连与住宅服务端配置，真起 `hysteria server`，
/// 同密码的 `hysteria client` 必须鉴权通过；**不带 obfs** 的客户端必须连不上，且在限时内以
/// 「failed to initialize client」退出（终态断言，不靠超时挂起判失败）。
///
/// 只换掉起内核必须换的几项：`listen`（跳跃区间要 root）、`trafficStats.listen`（别的用例
/// 并行时会撞 9999/9998）、`auth`（渲染出的 http 模式指向守护进程的 18789，测试里没有它，
/// 换成同形状的 `userpass`）。obfs / tls / 出站等其余字段原样用渲染结果。
#[test]
fn obfs_server_admits_a_matching_client_and_refuses_one_without_obfs() {
    if !common::have("hysteria") {
        eprintln!("skipped: hysteria not found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    if self_signed(dir.path()).is_none() {
        eprintln!("skipped: openssl not usable");
        return;
    }
    let s = common::state("obfs");
    let paths = Paths {
        base_dir: dir.path().to_path_buf(),
        certs_dir: dir.path().to_path_buf(),
        bin_dir: dir.path().join("bin"),
    };
    let slot0 = bui_schema::slots::resources(&s.node.ports, 0, 1);
    let obfs_pw = s.node.obfs.password.clone();
    for (which, rendered) in [
        (
            "direct",
            hysteria::direct_yaml(&s.node, &paths, Hy2Auth::Http),
        ),
        (
            "residential",
            hysteria::residential_slot_yaml(&s.node, &paths, &slot0, Hy2Auth::Http),
        ),
    ] {
        let port = free_udp_port();
        let mut text = with_listen(&rendered, &format!("127.0.0.1:{port}"));
        text = with_key(
            &text,
            "trafficStats",
            serde_yaml::from_str(&format!("listen: 127.0.0.1:{}", free_tcp_port())).unwrap(),
        );
        text = with_key(
            &text,
            "auth",
            serde_yaml::from_str("type: userpass\nuserpass:\n  alice: pw-e2e").unwrap(),
        );
        let y: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(y["obfs"]["type"].as_str(), Some("salamander"), "{which}");

        let server_cfg = dir.path().join(format!("server-{which}.yaml"));
        let server_log = dir.path().join(format!("server-{which}.log"));
        std::fs::write(&server_cfg, text).unwrap();
        let log = std::fs::File::create(&server_log).unwrap();
        let mut server = std::process::Command::new("hysteria")
            .args(["server", "--disable-update-check", "-c"])
            .arg(&server_cfg)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let up = wait_for_log(
            &server_log,
            "server up and running",
            &mut server,
            std::time::Duration::from_secs(5),
        );
        let client = |name: &str, obfs: Option<&str>| -> (std::path::PathBuf, std::path::PathBuf) {
            let mut c = format!(
                "server: 127.0.0.1:{port}\nauth: alice:pw-e2e\ntls:\n  sni: test.invalid\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:{}\n",
                free_tcp_port()
            );
            if let Some(pw) = obfs {
                c.push_str(&format!(
                    "obfs:\n  type: salamander\n  salamander:\n    password: {pw}\n"
                ));
            }
            let cfg = dir.path().join(format!("client-{which}-{name}.yaml"));
            std::fs::write(&cfg, c).unwrap();
            (cfg, dir.path().join(format!("client-{which}-{name}.log")))
        };
        let spawn = |cfg: &std::path::Path, log: &std::path::Path| {
            let f = std::fs::File::create(log).unwrap();
            std::process::Command::new("hysteria")
                .args(["client", "--disable-update-check", "-c"])
                .arg(cfg)
                .stdout(f.try_clone().unwrap())
                .stderr(f)
                .spawn()
                .unwrap()
        };

        // ① 不带 obfs：握手包服务端不认，客户端在 quic 握手超时（约 5 秒）后自己退出
        let (cfg, bad_log) = client("plain", None);
        let mut bad = spawn(&cfg, &bad_log);
        let start = std::time::Instant::now();
        let mut exited = None;
        while start.elapsed() < std::time::Duration::from_secs(20) {
            if let Some(st) = bad.try_wait().unwrap() {
                exited = Some(st);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if exited.is_none() {
            let _ = bad.kill();
        }
        let _ = bad.wait();
        let bad_out = std::fs::read_to_string(&bad_log).unwrap_or_default();
        let server_after_bad = std::fs::read_to_string(&server_log).unwrap_or_default();

        // ② 同密码 obfs：鉴权通过（客户端「connected to server」+ 服务端记下 alice）
        let (cfg, good_log) = client("obfs", Some(&obfs_pw));
        let mut good = spawn(&cfg, &good_log);
        let connected = wait_for_log(
            &good_log,
            "connected to server",
            &mut good,
            std::time::Duration::from_secs(10),
        );
        let admitted = wait_for_log(
            &server_log,
            "client connected",
            &mut server,
            std::time::Duration::from_secs(3),
        );
        for mut c in [good, server] {
            let _ = c.kill();
            let _ = c.wait();
        }
        let good_out = std::fs::read_to_string(&good_log).unwrap_or_default();
        let server_out = std::fs::read_to_string(&server_log).unwrap_or_default();

        assert!(up, "{which}：服务端没起来：{server_out}");
        let st = exited.unwrap_or_else(|| {
            panic!("{which}：不带 obfs 的客户端 20 秒内没有退出（挂起）：{bad_out}")
        });
        assert!(
            !st.success(),
            "{which}：不带 obfs 的客户端不该正常退出：{bad_out}"
        );
        assert!(
            bad_out.contains("failed to initialize client"),
            "{which}：不带 obfs 的客户端应以连不上退出：{bad_out}"
        );
        assert!(
            !bad_out.contains("connected to server"),
            "{which}：不带 obfs 竟然连上了：{bad_out}"
        );
        assert!(
            !server_after_bad.contains("client connected"),
            "{which}：服务端放进了不带 obfs 的客户端：{server_after_bad}"
        );
        assert!(
            connected,
            "{which}：同密码 obfs 客户端没连上：{good_out}\n{server_out}"
        );
        assert!(
            admitted && server_out.contains("\"id\": \"alice\""),
            "{which}：服务端没记下 alice 的会话：{server_out}"
        );
    }
}

/// 两种鉴权模式各一份 golden（`tests/fixtures/hysteria/<模式>/`）。
///
/// 逐字节比对而不是比 YAML 树：`auth` 段是唯一允许随模式变的地方，任何顺带的字段重排
/// （端口、伪装、sniff）都要在这里当场撞红 —— 切鉴权模式会重启两份 hysteria，
/// 顺手改别的字段等于把一次开关变成一次不可控的配置迁移。
#[test]
fn both_auth_modes_match_their_golden_config() {
    let s = common::state("obfs");
    let p = Paths::default_server();
    let slot0 = bui_schema::slots::resources(&s.node.ports, 0, 1);
    for (mode, auth) in [("http", Hy2Auth::Http), ("command", Hy2Auth::Command)] {
        assert_eq!(
            hysteria::direct_yaml(&s.node, &p, auth),
            golden(mode, "config.yaml"),
            "{mode} 模式的 config.yaml 与 golden 不符"
        );
        assert_eq!(
            hysteria::residential_slot_yaml(&s.node, &p, &slot0, auth),
            golden(mode, "config-residential.yaml"),
            "{mode} 模式的 config-residential.yaml 与 golden 不符"
        );
    }
}
