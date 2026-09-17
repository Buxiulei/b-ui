//! b-ui v4 共享 schema：期望态模型、节点集合、配置/订阅渲染、解析器、v3 导入。
//!
//! 本 crate 是 v4 的单一事实来源：服务端 `bui` 与客户端 `bui-c` 的所有配置、
//! 订阅与节点集合都由这里的纯函数从 [`State`] 派生，不在别处复制一份逻辑。
//! 所有渲染函数都不做 IO，输入是期望态、输出是字符串或 [`serde_json::Value`]。
//!
//! # 公共 API（总纲 C1 契约）
//!
//! ## 期望态模型 —— [`model`]
//!
//! [`State`] 是聚合根（对应 `/opt/b-ui/state.json`），其下：
//! [`NodeParams`](model::NodeParams)、[`Ports`](model::Ports)、[`Reality`](model::Reality)、
//! [`Obfs`](model::Obfs)、[`Admin`](model::Admin)、[`User`](model::User)、
//! [`Credentials`](model::Credentials)、[`Protocol`](model::Protocol)、
//! [`Entitlements`](model::Entitlements)、[`ResidentialEntitlement`](model::ResidentialEntitlement)、
//! [`TrafficLimit`](model::TrafficLimit)、[`Usage`](model::Usage)、[`PortalAuth`](model::PortalAuth)、
//! [`Billing`](model::Billing)、[`Order`](model::Order)、[`OrderStatus`](model::OrderStatus)、
//! [`Residential`](model::Residential)、[`ResidentialGroup`](model::ResidentialGroup)、
//! [`Hy2Pool`](model::Hy2Pool)、[`ReservedCred`](model::ReservedCred)、
//! [`ResiMode`](model::ResiMode)、[`Upstream`](model::Upstream)、[`UpstreamKind`](model::UpstreamKind)、
//! [`Verified`](model::Verified)、[`Blacklist`](model::Blacklist)、[`Rule`](model::Rule)、
//! [`Pin`](model::Pin)、[`AutoEntry`](model::AutoEntry)、
//! [`SystemSettings`](model::SystemSettings)、[`Versions`](model::Versions)、
//! [`CatalogItem`](model::CatalogItem)、[`CatalogKind`](model::CatalogKind)。
//!
//! ## 节点集合 —— [`nodes`]
//!
//! - [`nodes::nodes_for`]：按用户权限展开该用户可见的节点集合，返回
//!   [`Node`](nodes::Node)（[`NodeKind`](nodes::NodeKind) / [`Transport`](nodes::Transport)）。
//!
//! ## IP 池与槽位 —— [`slots`]
//!
//! - [`slots::SlotRes`] `{ index, relay_port }`：槽 i 的那**一个**端口（relay 的 socks
//!   入站 `2080 + i`，也是 xray 住宅出站的目标），由 [`slots::resources`] 从槽序号纯函数
//!   算出 —— **不吃 [`Ports`](model::Ports)**，因为 4.1 起槽位与对外端口无关。
//! - [`slots::sync_slots`] / [`slots::least_loaded`] / [`slots::assign`] /
//!   [`slots::migrate_unassigned`] / [`slots::rebalance`]：spec §5.6 的分配规则，纯函数。
//! - **没有「必须重新获取订阅」这回事了**（4.1，spec §1.2 目标 1）：住宅 HY2 只有一个
//!   监听端口、整段跳跃由 `table inet bui` 送进去，每个用户的端口与区间完全相同、与槽位
//!   无关，所以增删上游 / `assign` / `rebalance` 都不动已下发的订阅。4.0.x 那套按原因分
//!   三组的 `resubscribe_impact` / `ResubscribeImpact` 随之删除。
//! - 同一批删掉的还有按槽算对外端口的一整套（spec §4.2、§13 C1、§14 裁决 6，连签名一起，
//!   不留 `#[deprecated]` 也不留转发壳）：`SlotRes.{hy2_port, stats_port, hop}`、
//!   `slots::{HY2_STATS_RESI_BASE, hop_slice, slot_span, resources_of}`。
//!
//! ## 住宅 HY2 凭据池 —— [`hy2pool`]
//!
//! 住宅 HY2 是**一个** sing-box hysteria2 入站 + 一池静态凭据
//! （[`Hy2Pool`](model::Hy2Pool) / [`ReservedCred`](model::ReservedCred)，spec §3.1）：
//! 每条凭据一个门（selector `gate-<id>`），用户生命周期动作只切门、不改配置、不重启内核。
//!
//! - [`hy2pool::POOL_MIN`] / [`hy2pool::POOL_MAX`] / [`hy2pool::size_for`]：池容量 =
//!   `clamp(ceil16(2 × 住宅 hysteria2 用户数), 32, 256)`；基数由
//!   [`hy2pool::resi_hy2_users`] 数出。
//! - [`hy2pool::is_resi_hy2`]：「有住宅权益、权益指向的分组真实存在、且开了 hysteria2」
//!   ——池容量、迁移分凭据、门位收敛、面板投影、踢人与装完自检挑探测用户共用的**唯一**
//!   那条判据（权益被撤掉、或 `group_id` 悬空的持凭据用户都不满足它）。
//! - [`hy2pool::grow`]：补到目标条数（`id` = 最小空闲 `r%03d`，`name = id`）。
//! - [`hy2pool::assign_at`] / [`hy2pool::assign`] / [`hy2pool::release`] /
//!   [`hy2pool::cred_of`]：分配（先「从未用过」、再「`released_at` 最早且 ≥ 24 小时」；
//!   **幂等**，已持凭据的用户原样拿回那一条，换凭据必须显式 `release` + `assign`）、
//!   释放（记 `released_at`）与按用户取凭据。**生产一律走 `assign_at`**：24 小时冷却期是
//!   安全判据，判定时钟必须与盖 `released_at` 的那个同源（`bui` 侧的 `Host::now()`）；
//!   `assign` 是墙钟便利版，只给 bui-schema 自己的用例用。
//! - [`hy2pool::regenerate_idle_secrets`]：重写配置时顺带重随机全部空闲凭据的 `secret`。
//! - [`hy2pool::free_count`] / [`hy2pool::LOW_FREE_RATIO`]：空闲率与 20% 告警门槛。
//! - [`hy2pool::usage`] → [`hy2pool::PoolUsage`]：`bui status` 与
//!   `GET /api/residential/pool` 的**唯一**用量口径（`used + free == size`，
//!   悬空指针不计已用）。
//! - [`hy2pool::migrate`]：v4 → 4.1 一次性分配（迁移用户 `name = 用户名`、
//!   `secret = hy2_password` 的副本 ⇒ 订阅逐字不变），幂等；返回
//!   [`hy2pool::MigrateReport`]（`changed` / `unassigned`，后者非零时调用方打 Error 事件）。
//!
//! ## 订阅 token 与旧链接宽限期 —— [`sub`]
//!
//! - [`sub::new_sub_token`] / [`sub::is_sub_token`]：每用户随机订阅 token（32 位小写十六进制）
//!   的生成与严格判定，四个免鉴权端点按它在「token」与「用户名」之间分流。
//! - [`sub::LEGACY_SUB_GRACE_DAYS`] / [`sub::legacy_sub_deadline`]：旧「用户名链接」的宽限期。
//! - [`sub::sub_urls`]：把「域名 + token」拼成四条订阅地址（[`SubUrls`](sub::SubUrls)）。
//!
//! ## 解析器 —— [`parse`]
//!
//! - [`parse::upstream_url`]：把住宅上游的四种粘贴写法归一成 [`UpstreamInput`](parse::UpstreamInput)。
//! - [`parse::node_uri`](parse::node_uri())：把 `hysteria2://` / `vless://` 订阅 URI 解析回 [`Node`](nodes::Node)。
//! - 失败返回 [`ParseError`](parse::ParseError)。
//!
//! ## 分流关键字 —— [`keywords`]
//!
//! - [`keywords::DEFAULT_KEYWORDS`]：默认 AI 域名关键字表，与 v3 `residential-helper.sh`
//!   的 `DEFAULT_DOMAINS` 逐条一致。
//!
//! ## 服务端配置渲染 —— [`render`]
//!
//! - [`render::hysteria::direct_yaml`]：**直连** Hysteria2 实例的 `config.yaml`
//!   （4.1 起这是唯一一个 apernet hysteria 实例；4.0.x 的
//!   `residential_yaml` / `residential_slot_yaml` 与 `config-residential[-<i>].yaml`
//!   一起退役）。
//! - [`render::hy2_singbox::config`]：**住宅** HY2 的 `hy2-residential.json`（一个 sing-box
//!   hysteria2 入站 + 凭据池 + 每凭据一个 `gate-<id>` selector），端点常量
//!   [`render::hy2_singbox::HY2_RESI_CLASH_API`] / [`render::hy2_singbox::HY2_RESI_V2RAY_API`]
//!   与标签 [`render::hy2_singbox::INBOUND_TAG`] / [`render::hy2_singbox::DENY_TAG`] /
//!   [`render::hy2_singbox::gate_tag`] / [`render::hy2_singbox::slot_out_tag`] 都只有这一处来源。
//! - [`render::nft::ruleset`]：住宅 HY2 端口跳跃那张 `table inet bui`
//!   （[`render::nft::TABLE`]，整段 + 可选的 4.0 兼容段 REDIRECT 到单一监听端口）。
//! - [`render::xray::config`]：含 `vless-direct` / `vless-residential` 两个 REALITY 入站的
//!   `xray-config.json`；[`render::xray::structural_hash`] 忽略 `clients` 后取哈希，
//!   用于判断是否只是加减用户（可热更新而不必重启）。
//! - [`render::relay::config`]：本地 sing-box 中继 `singbox-relay.json`（池空时 fail-open 直连）。
//!
//! ## 订阅渲染 —— [`render::subscription`]
//!
//! - [`render::subscription::uri_list`]：v2rayN 用的 base64 URI 列表。
//! - [`render::subscription::singbox`]：完整 sing-box 配置（TUN + DNS + route）。
//! - [`render::subscription::clash`]：mihomo YAML。
//! - 三者的分流规则都来自 [`SplitRules`](render::SplitRules)（由
//!   [`SplitRules::from_group`](render::SplitRules::from_group) 从住宅分组派生）。
//!
//! ## 客户端配置渲染 —— [`render::client`]
//!
//! - [`render::client::tun_config`] / [`render::client::mixed_config`]：`bui-c` 的 TUN 模式与
//!   本地混合端口模式，选项见 [`ClientOpts`](render::client::ClientOpts)。
//! - [`render::client::probe_config`]：`bui-c` 测速配置，每个 [`ProbeTarget`](render::client::ProbeTarget)
//!   一个带认证（凭据由调用方随机生成）的 `127.0.0.1` socks 入站，按入站分流到各自节点；
//!   标签由函数按下标生成（出站 `probe-<i>`、入站 `probe-in-<i>`）。
//!
//! ## v3 导入 —— [`v3`]
//!
//! - [`v3::import`]：读取 v3 的 `/opt/b-ui` 目录，产出 [`ImportReport`](v3::ImportReport)
//!   （`state` + `warnings`），失败返回 [`ImportError`](v3::ImportError)。
//! - [`v3::direct_entitlement`]：按 v3 协议名 + 住宅开关算直连权益（单协议开住宅 ⇒ 无直连），
//!   导入与面板新建用户共用，保证订阅与 v3 逐项等价。
//!
//! ## 路径 —— [`paths`]
//!
//! - [`Paths`](paths::Paths)：渲染器需要写进配置里的绝对路径，
//!   服务端默认值见 [`Paths::default_server`](paths::Paths::default_server)。
//!
//! # 用法
//!
//! ```
//! use bui_schema::State;
//!
//! // State 是唯一聚合根：所有渲染器都只从它（及其子结构）派生产物。
//! fn render_everything(_state: &State) { /* ... */ }
//! ```

pub mod hy2pool;
pub mod keywords;
pub mod model;
pub mod nodes;
pub mod parse;
pub mod paths;
pub mod render;
pub mod slots;
pub mod sub;
pub mod v3;

pub use model::State;
