# 住宅代理稳定性回归（2026-09-28）

本轮基于 v4 / 4.1.3 修复住宅 NAT、故障切换时序，并将随发布分发的 sing-box 从
1.14.1 升级到 1.14.2。没有修改直连 apernet Hysteria 内核或凭据格式。

## 已复现的问题

1. `inet bui` 只匹配 UDP 目的端口，OUTPUT 会把发往外部同号端口的数据送回本机，
   PREROUTING 也会误截经本机转发的流量。规则加入 `fib daddr type local`；watchdog
   将此条件纳入规范化比较，能够重放旧规则且不重启服务。
2. 住宅巡检曾在选路之前执行整池吞吐测速，失效出口会继续等待测速。现先完成故障
   切换，再测速；成员快照检查也提前到槽位操作之前。慢轮次结束后重置间隔，避免补跑
   积压轮次使迟滞、恢复判定过快。
3. sing-box 1.14.1 的 selector 包装 UDP 连接时丢失完整的包连接接口。同一 SOCKS5
   UDP 关联先发送 IPv4 目标、再发送域名目标时，日志出现
   `packet upload closed: unsupported address`，第二个包无法送达模拟上游。
   1.14.2 在同配置、同报文下成功。

第 3 项与上游 [UDP 出站组修复](https://github.com/SagerNet/sing-box/commit/ca7216a3811f0f99d52ffda911a114178721dbae)
对应。不能将它扩展为“所有 UDP 都失败”：每次建立新关联、只发送已解析的 IPv4 目标时，
1.14.1 也可以成功。这也是只做上游 STUN 探测或配置语法校验不足以发现该故障的原因。

## 回归入口

```bash
# Linux、有 root/CAP_SYS_ADMIN/CAP_NET_ADMIN、nft/ip/python3；隔离网络内收发，无外网访问
sudo -E cargo test -p bui-schema --test kernel_nft -- --ignored --nocapture

# PATH 上的 sing-box 必须为当前发布内核；只监听回环、使用合成 SOCKS5 上游与 hosts DNS
cargo test -p bui-schema --test kernel_traffic -- --ignored --nocapture
```

`kernel_nft` 先装入旧规则复现误截，再验证 IPv4/IPv6 的外部、转发、回环、本机地址、
入站跳跃段和兼容端口。CI 有独立的 namespace job。

`kernel_traffic` 使用 `render::relay::config` 生成配置，只调整监听端口、移除无关的
管理/缓存接口，并以本地 hosts DNS 代替外网 DNS。测试保持实际选路规则、SOCKS 出站
和槽位 selector，验证：

- 同一 UDP 关联中的 IPv4 → 域名目标变化及双向报文；后续报文不会重新执行首包路由。
- 新关联以域名为首包时，现有 IPv4 解析规则仍生效。

将 PATH 指向 1.14.1 时，该测试应失败并出现上述错误；1.14.2 应通过。CI 的 1.14
发布内核格实际执行此测试，1.12/1.13 继续承担旧客户端配置兼容校验。

## 内核升级验证

- 用 `pin-kernels.sh --write` 生成新锁，未跳过 `--check` 门禁。
- Go 仍锁定 go1.25.5；amd64/arm64 两次独立构建的 SHA-256 分别与锁一致。
- 自建 1.14.2 的标签完整包含官方 1.14.2 标签，并额外包含 `with_v2ray_api`。
- 使用新内核的 Linux 全工作区测试：1926 项通过，3 项默认忽略；新增 UDP 数据面测试
  单独执行通过，旧内核负对照失败。格式、Clippy、版本轨道检查通过。

这些验证证明了具体缺陷及修复路径，不等同于长期网络稳定性保证。直连 HY2 的
`no recent network activity` 仍需与故障时刻的客户端日志、网络切换及服务端状态关联，
不能凭该日志条数认定独立断线次数，也不应无证据地调整 QUIC 超时或重启健康服务。
