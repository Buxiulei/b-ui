//! 生成 Xray（`proto/`）与住宅 HY2 计量用的 sing-box `daemon.StartedService`（`proto-singbox/`）
//! 的 gRPC 客户端代码。
//!
//! 三个要点，改动前先读：
//! 1. `protoc` 由 `protoc-bin-vendored` 提供，本机与 CI 都不需要装 protobuf-compiler。
//! 2. 必须用 `include_file`：prost 生成的跨包引用形如 `super::super::super::common::serial::TypedMessage`，
//!    只有把模块树按 proto 的 package 层级嵌套起来才解析得通。`include_file` 生成的
//!    `xray.rs` 就是那棵树，`xray.rs` 里再 `include!` 各包的实现文件。
//! 3. 两棵 proto 树**分开编译**（各自一次 `configure()`、各自一个 `include_file`）：
//!    prost 把每个 package 写成同一个 `OUT_DIR` 下的 `<package>.rs`，而一棵树只能有一个
//!    模块根；混在一次编译里，两边的 package / 消息同名时只会互相覆盖。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // edition 2021 下 `set_var` 是安全函数；build 脚本单线程，改成 edition 2024 时
    // 这一行要套 `unsafe`（见 rust#27970 的稳定化说明）。
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .include_file("xray.rs")
        .compile_protos(
            &[
                "proto/app/proxyman/command/command.proto",
                "proto/app/stats/command/command.proto",
                "proto/common/protocol/user.proto",
                "proto/common/serial/typed_message.proto",
                "proto/core/config.proto",
                "proto/proxy/vless/account.proto",
                "proto/app/proxyman/config.proto",
                "proto/transport/internet/config.proto",
                "proto/common/net/address.proto",
                "proto/common/net/port.proto",
                "proto/app/router/command/command.proto",
                "proto/app/router/config.proto",
                "proto/common/net/network.proto",
            ],
            &["proto"],
        )?;
    println!("cargo:rerun-if-changed=proto");
    // 住宅 HY2 的计量面（4.1 起官方内核自带）：`daemon.StartedService` 的
    // SubscribeConnections / SubscribeStatus / GetStartedAt。整棵 vendor 树只有这一个文件，
    // 与上游 tag 逐字节一致（`scripts/release/check-vendored-proto.sh` 守这条）。
    // ⚠️ include_file 的名字用 `singbox_daemon.rs`，别写成 `daemon.rs`：proto 的 package 就叫
    // `daemon`，prost 生成的包文件也叫 `daemon.rs`，同名会自己 include 自己（无限递归）。
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .include_file("singbox_daemon.rs")
        .compile_protos(
            &["proto-singbox/daemon/started_service.proto"],
            &["proto-singbox"],
        )?;
    println!("cargo:rerun-if-changed=proto-singbox");
    Ok(())
}
