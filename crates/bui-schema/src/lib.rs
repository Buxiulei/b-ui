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
//! - [`render::hysteria::direct_yaml`] / [`render::hysteria::residential_yaml`]：两个
//!   Hysteria2 实例的 `config.yaml` / `config-residential.yaml`。
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
//!
//! ## v3 导入 —— [`v3`]
//!
//! - [`v3::import`]：读取 v3 的 `/opt/b-ui` 目录，产出 [`ImportReport`](v3::ImportReport)
//!   （`state` + `warnings`），失败返回 [`ImportError`](v3::ImportError)。
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

pub mod keywords;
pub mod model;
pub mod nodes;
pub mod parse;
pub mod paths;
pub mod render;
pub mod v3;

pub use model::State;
