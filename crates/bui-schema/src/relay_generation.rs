//! 不可变住宅出口代的纯发布协议；尚未接入生产 driver。
//!
//! 这里不发起 IO、不猜测运行状态，也不把 Clash 空连接表当作完成在途 dial 的屏障。
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

/// 两组互不重叠的策略监听资源；bank 只在旧代确认停止后才可复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bank {
    A,
    B,
}

impl Bank {
    pub fn policy_port(self, index: usize) -> Option<u16> {
        (index < usize::from(crate::slots::MAX_SLOTS)).then(|| match self {
            Self::A => 2180 + index as u16,
            Self::B => 2280 + index as u16,
        })
    }

    pub fn api(self) -> &'static str {
        match self {
            Self::A => "127.0.0.1:9093",
            Self::B => "127.0.0.1:9094",
        }
    }

    pub fn member_tag(self, id: Uuid) -> String {
        let bank = match self {
            Self::A => "a",
            Self::B => "b",
        };
        format!("resi-policy-bank-{bank}-{id}")
    }
}

/// 严格小写 SHA-256；代与验收产物必须使用完整内容指纹。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Sha256(String);

impl Sha256 {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ProtocolError::InvalidSha);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// 此发布协议只允许固定入口、固定内核、同 UUID 同顺序的上游更新。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationLayout {
    pub front_sha256: Sha256,
    pub kernel_sha256: Sha256,
    #[serde(deserialize_with = "canonical_uuid_vec")]
    pub upstream_ids: Vec<Uuid>,
}

/// 编译候选配置前的身份预览；不占 bank，真正预留时必须再次匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationIdentity {
    pub generation: u64,
    pub bank: Bank,
}

/// 不可变的代身份；复用 bank 不会复用单调代号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationBinding {
    pub generation: u64,
    pub bank: Bank,
    pub config_sha256: Sha256,
    pub kernel_sha256: Sha256,
    pub front_sha256: Sha256,
    #[serde(deserialize_with = "canonical_uuid_vec")]
    pub upstream_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeasePhase {
    Preparing,
    Ready,
    Publishing,
    Active,
    Draining,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRef {
    pub generation: u64,
    pub bank: Bank,
    #[serde(deserialize_with = "canonical_uuid")]
    pub upstream_id: Uuid,
}

/// Unknown 必须显式保留；缺失、坏响应不可被转换成 Direct。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemberChoice {
    Unknown,
    Direct,
    Policy(PolicyRef),
}

/// 同一个运行入口实例中取得的 boot 与实际 selector 快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontView {
    pub front_sha256: Sha256,
    #[serde(deserialize_with = "canonical_uuid")]
    pub instance: Uuid,
    /// driver 管理的观测版本，涵盖 boot 与 actual 的同一观测批次。
    /// 已知映射改变或知识失效时必须递增；同版本只允许 Unknown 补成已知。
    /// 此数字本身不是内核提供的 dial 屏障。
    pub selector_epoch: u64,
    pub boot: BTreeMap<String, MemberChoice>,
    pub actual: BTreeMap<String, MemberChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishIntent {
    members: BTreeMap<String, MemberChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BankLease {
    binding: GenerationBinding,
    phase: LeasePhase,
    #[serde(deserialize_with = "required_option")]
    publish: Option<PublishIntent>,
}

impl BankLease {
    pub fn binding(&self) -> &GenerationBinding {
        &self.binding
    }
    pub fn phase(&self) -> LeasePhase {
        self.phase
    }
}

/// 不属于 runtime 缓存。写盘/原子提交由未来 driver 负责；损坏快照不能默认重置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GenerationLedger {
    schema_version: u32,
    revision: u64,
    next_generation: u64,
    layout: GenerationLayout,
    banks: BTreeMap<Bank, BankLease>,
    front_view: Option<FrontView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerWire {
    schema_version: u32,
    revision: u64,
    next_generation: u64,
    layout: GenerationLayout,
    banks: BTreeMap<Bank, BankLease>,
    #[serde(deserialize_with = "required_option")]
    front_view: Option<FrontView>,
}

/// 候选 config check、内核校验、实际 API 清单与每上游探测的同代绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEvidence {
    pub binding: GenerationBinding,
    pub kernel_version: String,
    pub checked: bool,
    pub api_upstreams: BTreeSet<Uuid>,
    pub probed_upstreams: BTreeSet<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowObservation {
    pub generation: u64,
    pub tcp: Option<u64>,
    pub udp: Option<u64>,
    pub handshakes: Option<u64>,
}

/// 只有前端真正完成该 epoch 前所有在途 TCP/UDP dial 后才能构造。
/// API 空连接表、等待固定时长、后端零连接都不能提供此证明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontDialBarrier {
    pub generation: u64,
    pub front_instance: Uuid,
    pub selector_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimEvidence {
    pub selectors: FrontView,
    pub flows: FlowObservation,
    pub dial_barrier: Option<FrontDialBarrier>,
}

/// 仅当前协议修订允许消费，不能跨进程恢复后重放旧 stop 决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAuthorization {
    binding: GenerationBinding,
    ledger_revision: u64,
    selectors: FrontView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopEvidence {
    pub binding: GenerationBinding,
    pub command_succeeded: bool,
    pub observed_inactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishProgress {
    Partial,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("SHA-256 must be exactly 64 lowercase hexadecimal characters")]
    InvalidSha,
    #[error("invalid generation layout: {0}")]
    InvalidLayout(String),
    #[error("corrupt generation ledger: {0}")]
    CorruptSnapshot(String),
    #[error(
        "bank resources are unavailable; wait for verified drain or finish stopped-lease cleanup"
    )]
    NeedDrain,
    #[error("a candidate generation is already in progress")]
    CandidateInProgress,
    #[error("bank has no lease")]
    BankMissing,
    #[error("operation is not allowed in this phase")]
    WrongPhase,
    #[error("front, kernel or ordered suppliers changed; maintenance required")]
    MaintenanceRequired,
    #[error("evidence does not bind to the leased generation")]
    EvidenceMismatch,
    #[error("selector state is missing or unknown")]
    UnknownObservation,
    #[error("generation is still referenced by boot or actual selectors")]
    StillReferenced,
    #[error("TCP, UDP or handshakes are not fully observed at zero")]
    NotDrained,
    #[error("completed front dial barrier is missing or does not match the snapshot")]
    BarrierMissing,
    #[error("stop did not succeed or backend is not confirmed inactive")]
    StopFailed,
    #[error("stop authorization is stale")]
    StaleAuthorization,
    #[error("generation or revision counter exhausted")]
    CounterExhausted,
    #[error("reservation identity changed before the compiled config was leased")]
    StaleReservation,
}

fn canonical_uuid<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    let value = String::deserialize(d)?;
    let id = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
    if id.is_nil() || id.to_string() != value {
        return Err(serde::de::Error::custom(
            "UUID must be non-nil and canonical",
        ));
    }
    Ok(id)
}

fn canonical_uuid_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Uuid>, D::Error> {
    let values = Vec::<String>::deserialize(d)?;
    values
        .into_iter()
        .map(|value| {
            let id = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
            if id.is_nil() || id.to_string() != value {
                return Err(serde::de::Error::custom(
                    "UUID must be non-nil and canonical",
                ));
            }
            Ok(id)
        })
        .collect()
}

fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

/// 先验证整棵 JSON 的 key 唯一，再让结构反序列化执行字段/关系验证。
/// serde 的普通 BTreeMap 接受重复 key，不能承担租约持久化边界。
struct UnambiguousJson(serde_json::Value);

impl<'de> Deserialize<'de> for UnambiguousJson {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UnambiguousJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UnambiguousJson(serde_json::Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(UnambiguousJson(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(UnambiguousJson(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(UnambiguousJson(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                let n = serde_json::Number::from_f64(v)
                    .ok_or_else(|| E::custom("non-finite number"))?;
                Ok(UnambiguousJson(serde_json::Value::Number(n)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UnambiguousJson(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(UnambiguousJson(v.into()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = seq.next_element::<UnambiguousJson>()? {
                    values.push(v.0);
                }
                Ok(UnambiguousJson(serde_json::Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate JSON key: {key}"
                        )));
                    }
                    values.insert(key, map.next_value::<UnambiguousJson>()?.0);
                }
                Ok(UnambiguousJson(serde_json::Value::Object(values)))
            }
        }
        d.deserialize_any(Visitor)
    }
}

impl<'de> Deserialize<'de> for GenerationLedger {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = UnambiguousJson::deserialize(d)?;
        let wire: LedgerWire = serde_json::from_value(raw.0).map_err(serde::de::Error::custom)?;
        let ledger = Self {
            schema_version: wire.schema_version,
            revision: wire.revision,
            next_generation: wire.next_generation,
            layout: wire.layout,
            banks: wire.banks,
            front_view: wire.front_view,
        };
        ledger.validate().map_err(serde::de::Error::custom)?;
        Ok(ledger)
    }
}

impl GenerationLedger {
    pub fn new(layout: GenerationLayout) -> Result<Self, ProtocolError> {
        Self::validate_layout(&layout)?;
        Ok(Self {
            schema_version: 1,
            revision: 0,
            next_generation: 1,
            layout,
            banks: BTreeMap::new(),
            front_view: None,
        })
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ProtocolError> {
        serde_json::from_slice(bytes).map_err(|e| ProtocolError::CorruptSnapshot(e.to_string()))
    }

    pub fn to_json(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        serde_json::to_vec_pretty(self).map_err(|e| ProtocolError::CorruptSnapshot(e.to_string()))
    }

    pub fn lease(&self, bank: Bank) -> Option<&BankLease> {
        self.banks.get(&bank)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn check_layout(&self, layout: &GenerationLayout) -> Result<(), ProtocolError> {
        if &self.layout != layout {
            return Err(ProtocolError::MaintenanceRequired);
        }
        Ok(())
    }

    /// 先取得身份，再使用纯 backend compiler 生成字节并计算 SHA。
    /// 预览没有占用资源；驱动器持久化 reserve_expected 之后才能启动候选。
    /// Stopped 租约必须先完成 free_stopped；清理完成后才允许下一代准备，
    /// 避免旧停止证明与新代未知/部分发布观测共用同一份 front_view。
    pub fn reservation_identity(&self) -> Result<ReservationIdentity, ProtocolError> {
        if self
            .banks
            .values()
            .any(|lease| lease.phase == LeasePhase::Stopped)
        {
            return Err(ProtocolError::NeedDrain);
        }
        let bank = [Bank::A, Bank::B]
            .into_iter()
            .find(|bank| !self.banks.contains_key(bank))
            .ok_or(ProtocolError::NeedDrain)?;
        if self.banks.values().any(|lease| {
            matches!(
                lease.phase,
                LeasePhase::Preparing | LeasePhase::Ready | LeasePhase::Publishing
            )
        }) {
            return Err(ProtocolError::CandidateInProgress);
        }
        self.next_generation
            .checked_add(1)
            .ok_or(ProtocolError::CounterExhausted)?;
        Ok(ReservationIdentity {
            generation: self.next_generation,
            bank,
        })
    }

    pub fn reserve_expected(
        &mut self,
        identity: ReservationIdentity,
        config: Sha256,
    ) -> Result<GenerationBinding, ProtocolError> {
        self.transaction(|next| {
            if next.reservation_identity()? != identity {
                return Err(ProtocolError::StaleReservation);
            }
            next.next_generation = identity
                .generation
                .checked_add(1)
                .ok_or(ProtocolError::CounterExhausted)?;
            let binding = GenerationBinding {
                generation: identity.generation,
                bank: identity.bank,
                config_sha256: config,
                kernel_sha256: next.layout.kernel_sha256.clone(),
                front_sha256: next.layout.front_sha256.clone(),
                upstream_ids: next.layout.upstream_ids.clone(),
            };
            next.banks.insert(
                identity.bank,
                BankLease {
                    binding: binding.clone(),
                    phase: LeasePhase::Preparing,
                    publish: None,
                },
            );
            Ok(binding)
        })
    }

    pub fn reserve(&mut self, config: Sha256) -> Result<GenerationBinding, ProtocolError> {
        let identity = self.reservation_identity()?;
        self.reserve_expected(identity, config)
    }

    pub fn mark_ready(
        &mut self,
        bank: Bank,
        evidence: &PreparedEvidence,
    ) -> Result<(), ProtocolError> {
        self.transaction(|next| {
            let lease = next
                .banks
                .get_mut(&bank)
                .ok_or(ProtocolError::BankMissing)?;
            if lease.phase != LeasePhase::Preparing {
                return Err(ProtocolError::WrongPhase);
            }
            let expected = lease
                .binding
                .upstream_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            if evidence.binding != lease.binding
                || evidence.kernel_version != "1.14.2"
                || !evidence.checked
                || evidence.api_upstreams != expected
                || evidence.probed_upstreams != expected
            {
                return Err(ProtocolError::EvidenceMismatch);
            }
            lease.phase = LeasePhase::Ready;
            Ok(())
        })
    }

    pub fn begin_publish(&mut self, bank: Bank) -> Result<(), ProtocolError> {
        self.transaction(|next| {
            let lease = next
                .banks
                .get_mut(&bank)
                .ok_or(ProtocolError::BankMissing)?;
            if lease.phase != LeasePhase::Ready {
                return Err(ProtocolError::WrongPhase);
            }
            // 驱动器必须持久化本次意图，再发出第一条成员切换请求。
            lease.publish = Some(PublishIntent {
                members: Self::members(&lease.binding),
            });
            lease.phase = LeasePhase::Publishing;
            Ok(())
        })
    }

    pub fn observe_publish(
        &mut self,
        bank: Bank,
        view: &FrontView,
    ) -> Result<PublishProgress, ProtocolError> {
        self.transaction(|next| {
            let lease = next.banks.get(&bank).ok_or(ProtocolError::BankMissing)?;
            if lease.phase != LeasePhase::Publishing {
                return Err(ProtocolError::WrongPhase);
            }
            next.validate_view(view, false)?;
            let normalized = next.normalize_view(view);
            let complete = lease
                .publish
                .as_ref()
                .is_some_and(|intent| intent.members == normalized.actual);
            next.front_view = Some(normalized);
            if !complete {
                return Ok(PublishProgress::Partial);
            }
            for (&other_bank, other) in &mut next.banks {
                if other_bank != bank && other.phase == LeasePhase::Active {
                    other.phase = LeasePhase::Draining;
                }
            }
            next.banks.get_mut(&bank).expect("validated lease").phase = LeasePhase::Active;
            Ok(PublishProgress::Active)
        })
    }

    pub fn authorize_stop(
        &self,
        bank: Bank,
        evidence: &ReclaimEvidence,
    ) -> Result<StopAuthorization, ProtocolError> {
        let lease = self.banks.get(&bank).ok_or(ProtocolError::BankMissing)?;
        if lease.phase != LeasePhase::Draining {
            return Err(ProtocolError::WrongPhase);
        }
        self.unreferenced(&lease.binding, &evidence.selectors)?;
        if evidence.flows.generation != lease.binding.generation {
            return Err(ProtocolError::EvidenceMismatch);
        }
        if evidence.flows.tcp != Some(0)
            || evidence.flows.udp != Some(0)
            || evidence.flows.handshakes != Some(0)
        {
            return Err(ProtocolError::NotDrained);
        }
        let expected = FrontDialBarrier {
            generation: lease.binding.generation,
            front_instance: evidence.selectors.instance,
            selector_epoch: evidence.selectors.selector_epoch,
        };
        if evidence.dial_barrier.as_ref() != Some(&expected) {
            return Err(ProtocolError::BarrierMissing);
        }
        Ok(StopAuthorization {
            binding: lease.binding.clone(),
            ledger_revision: self.revision,
            selectors: evidence.selectors.clone(),
        })
    }

    pub fn mark_stopped(
        &mut self,
        bank: Bank,
        authorization: &StopAuthorization,
        evidence: &StopEvidence,
    ) -> Result<(), ProtocolError> {
        self.transaction(|next| {
            if authorization.ledger_revision != next.revision {
                return Err(ProtocolError::StaleAuthorization);
            }
            let lease = next.banks.get(&bank).ok_or(ProtocolError::BankMissing)?;
            if lease.phase != LeasePhase::Draining {
                return Err(ProtocolError::WrongPhase);
            }
            if authorization.binding != lease.binding || evidence.binding != lease.binding {
                return Err(ProtocolError::EvidenceMismatch);
            }
            Self::verify_stop(evidence)?;
            next.front_view = Some(authorization.selectors.clone());
            next.banks.get_mut(&bank).expect("validated lease").phase = LeasePhase::Stopped;
            Ok(())
        })
    }

    pub fn discard_candidate(
        &mut self,
        bank: Bank,
        view: &FrontView,
        evidence: &StopEvidence,
    ) -> Result<(), ProtocolError> {
        self.transaction(|next| {
            let lease = next.banks.get(&bank).ok_or(ProtocolError::BankMissing)?;
            if !matches!(lease.phase, LeasePhase::Preparing | LeasePhase::Ready) {
                return Err(ProtocolError::WrongPhase);
            }
            next.unreferenced(&lease.binding, view)?;
            if evidence.binding != lease.binding {
                return Err(ProtocolError::EvidenceMismatch);
            }
            Self::verify_stop(evidence)?;
            next.front_view = Some(view.clone());
            next.banks.get_mut(&bank).expect("validated lease").phase = LeasePhase::Stopped;
            Ok(())
        })
    }

    pub fn free_stopped(&mut self, bank: Bank, view: &FrontView) -> Result<(), ProtocolError> {
        self.transaction(|next| {
            let lease = next.banks.get(&bank).ok_or(ProtocolError::BankMissing)?;
            if lease.phase != LeasePhase::Stopped {
                return Err(ProtocolError::WrongPhase);
            }
            next.unreferenced(&lease.binding, view)?;
            next.front_view = Some(view.clone());
            next.banks.remove(&bank);
            Ok(())
        })
    }

    /// 唯一 tag 解析器提供代号与 UUID；只有最新实际选择证明的当前代参与策略学习。
    pub fn current_policy_for_log(
        &self,
        generation: u64,
        upstream: Uuid,
        view: &FrontView,
    ) -> Option<&GenerationBinding> {
        self.validate_view(view, false).ok()?;
        if self.front_view.as_ref()?.instance != view.instance {
            return None;
        }
        let lease = self
            .banks
            .values()
            .find(|lease| lease.binding.generation == generation)?;
        if !matches!(lease.phase, LeasePhase::Active | LeasePhase::Publishing) {
            return None;
        }
        let index = lease
            .binding
            .upstream_ids
            .iter()
            .position(|id| *id == upstream)?;
        let member = format!("resi-{}", index + 1);
        let expected = MemberChoice::Policy(PolicyRef {
            generation,
            bank: lease.binding.bank,
            upstream_id: upstream,
        });
        if lease.publish.as_ref()?.members.get(&member) != Some(&expected)
            || view.actual.get(&member) != Some(&expected)
        {
            return None;
        }
        Some(&lease.binding)
    }

    fn verify_stop(evidence: &StopEvidence) -> Result<(), ProtocolError> {
        if !evidence.command_succeeded || !evidence.observed_inactive {
            return Err(ProtocolError::StopFailed);
        }
        Ok(())
    }

    fn transaction<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        let mut next = self.clone();
        let result = f(&mut next)?;
        next.revision = self
            .revision
            .checked_add(1)
            .ok_or(ProtocolError::CounterExhausted)?;
        next.validate()?;
        *self = next;
        Ok(result)
    }

    fn members(binding: &GenerationBinding) -> BTreeMap<String, MemberChoice> {
        binding
            .upstream_ids
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                (
                    format!("resi-{}", i + 1),
                    MemberChoice::Policy(PolicyRef {
                        generation: binding.generation,
                        bank: binding.bank,
                        upstream_id: id,
                    }),
                )
            })
            .collect()
    }

    fn normalize_view(&self, view: &FrontView) -> FrontView {
        let mut normalized = view.clone();
        for i in 0..self.layout.upstream_ids.len() {
            let key = format!("resi-{}", i + 1);
            normalized
                .boot
                .entry(key.clone())
                .or_insert(MemberChoice::Unknown);
            normalized
                .actual
                .entry(key)
                .or_insert(MemberChoice::Unknown);
        }
        normalized
    }

    fn validate_view(&self, view: &FrontView, require_known: bool) -> Result<(), ProtocolError> {
        if view.front_sha256 != self.layout.front_sha256
            || view.instance.is_nil()
            || view.selector_epoch == 0
        {
            return Err(ProtocolError::EvidenceMismatch);
        }
        if let Some(last) = &self.front_view {
            if last.instance == view.instance {
                if last.selector_epoch > view.selector_epoch {
                    return Err(ProtocolError::EvidenceMismatch);
                }
                if last.selector_epoch == view.selector_epoch {
                    for (previous, current) in
                        [(&last.boot, &view.boot), (&last.actual, &view.actual)]
                    {
                        for (member, known) in previous {
                            if *known != MemberChoice::Unknown && current.get(member) != Some(known)
                            {
                                return Err(ProtocolError::EvidenceMismatch);
                            }
                        }
                    }
                }
            }
        }
        let keys: BTreeSet<_> = (1..=self.layout.upstream_ids.len())
            .map(|i| format!("resi-{i}"))
            .collect();
        for map in [&view.boot, &view.actual] {
            if map.keys().any(|key| !keys.contains(key)) {
                return Err(ProtocolError::EvidenceMismatch);
            }
            if require_known
                && (map.len() != keys.len()
                    || map.values().any(|choice| *choice == MemberChoice::Unknown))
            {
                return Err(ProtocolError::UnknownObservation);
            }
            for (member, choice) in map {
                let MemberChoice::Policy(reference) = choice else {
                    continue;
                };
                let index = member
                    .strip_prefix("resi-")
                    .and_then(|value| value.parse::<usize>().ok())
                    .and_then(|i| i.checked_sub(1))
                    .ok_or(ProtocolError::EvidenceMismatch)?;
                let lease = self
                    .banks
                    .get(&reference.bank)
                    .ok_or(ProtocolError::EvidenceMismatch)?;
                if reference.generation != lease.binding.generation
                    || self.layout.upstream_ids.get(index) != Some(&reference.upstream_id)
                {
                    return Err(ProtocolError::EvidenceMismatch);
                }
            }
        }
        Ok(())
    }

    fn unreferenced(
        &self,
        binding: &GenerationBinding,
        view: &FrontView,
    ) -> Result<(), ProtocolError> {
        self.validate_view(view, true)?;
        if view.boot.values().chain(view.actual.values()).any(|choice| matches!(choice, MemberChoice::Policy(reference) if reference.generation == binding.generation)) { return Err(ProtocolError::StillReferenced); }
        Ok(())
    }

    fn validate_layout(layout: &GenerationLayout) -> Result<(), ProtocolError> {
        if layout.upstream_ids.is_empty()
            || layout.upstream_ids.len() > usize::from(crate::slots::MAX_SLOTS)
            || layout.upstream_ids.iter().any(Uuid::is_nil)
            || layout.upstream_ids.iter().collect::<BTreeSet<_>>().len()
                != layout.upstream_ids.len()
        {
            return Err(ProtocolError::InvalidLayout(
                "requires one to eight distinct non-nil supplier UUIDs".into(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        let invalid = |reason: &str| ProtocolError::CorruptSnapshot(reason.into());
        Self::validate_layout(&self.layout).map_err(|e| invalid(&e.to_string()))?;
        if self.schema_version != 1 || self.next_generation == 0 {
            return Err(invalid("unsupported schema or invalid next generation"));
        }
        let mut numbers = BTreeSet::new();
        let mut candidate_count = 0;
        let mut active_count = 0;
        let mut draining_count = 0;
        let mut stopped_count = 0;
        for (&bank, lease) in &self.banks {
            let binding = &lease.binding;
            if binding.bank != bank
                || binding.generation == 0
                || binding.generation >= self.next_generation
                || !numbers.insert(binding.generation)
                || binding.front_sha256 != self.layout.front_sha256
                || binding.kernel_sha256 != self.layout.kernel_sha256
                || binding.upstream_ids != self.layout.upstream_ids
            {
                return Err(invalid(
                    "lease identity, digest, UUID order or counter mismatch",
                ));
            }
            match lease.phase {
                LeasePhase::Preparing | LeasePhase::Ready => {
                    candidate_count += 1;
                    if lease.publish.is_some() {
                        return Err(invalid("unpublished candidate has publish intent"));
                    }
                }
                LeasePhase::Publishing => {
                    candidate_count += 1;
                    if lease.publish.is_none() {
                        return Err(invalid("publishing generation has no intent"));
                    }
                }
                LeasePhase::Active => {
                    active_count += 1;
                    if lease.publish.is_none() || self.front_view.is_none() {
                        return Err(invalid("active generation has no observed publication"));
                    }
                }
                LeasePhase::Draining => {
                    draining_count += 1;
                    if lease.publish.is_none() || self.front_view.is_none() {
                        return Err(invalid("draining generation has no observed publication"));
                    }
                }
                LeasePhase::Stopped => {
                    stopped_count += 1;
                }
            }
            if let Some(intent) = &lease.publish {
                if intent.members != Self::members(binding) {
                    return Err(invalid("publish intent does not match immutable lease"));
                }
            }
        }
        if candidate_count > 1 || active_count > 1 || draining_count > 1 {
            return Err(invalid(
                "multiple candidate, active or draining generations",
            ));
        }
        if stopped_count > 1 || (stopped_count > 0 && candidate_count > 0) {
            return Err(invalid(
                "stopped leases must be freed before preparing another generation",
            ));
        }
        let active_generation = self
            .banks
            .values()
            .find(|lease| lease.phase == LeasePhase::Active)
            .map(|lease| lease.binding.generation);
        if draining_count == 1 && active_generation.is_none() {
            return Err(invalid(
                "draining generation requires a newer active generation",
            ));
        }
        if let Some(active) = active_generation {
            for lease in self.banks.values() {
                if lease.phase == LeasePhase::Draining && lease.binding.generation >= active {
                    return Err(invalid(
                        "draining generation is not older than active generation",
                    ));
                }
                if matches!(
                    lease.phase,
                    LeasePhase::Preparing | LeasePhase::Ready | LeasePhase::Publishing
                ) && lease.binding.generation <= active
                {
                    return Err(invalid(
                        "candidate generation is not newer than active generation",
                    ));
                }
            }
        }
        for lease in self
            .banks
            .values()
            .filter(|lease| lease.phase == LeasePhase::Stopped)
        {
            match (lease.publish.is_some(), active_generation) {
                (true, Some(active)) if lease.binding.generation < active => {}
                (true, _) => {
                    return Err(invalid(
                        "published stopped generation requires a newer active generation",
                    ))
                }
                (false, Some(active)) if lease.binding.generation <= active => {
                    return Err(invalid(
                        "failed candidate stopped generation is not newer than active generation",
                    ))
                }
                (false, _) => {}
            }
            let view = self
                .front_view
                .as_ref()
                .ok_or_else(|| invalid("stopped generation lacks selector observations"))?;
            self.unreferenced(&lease.binding, view)
                .map_err(|e| invalid(&e.to_string()))?;
        }
        if let Some(view) = &self.front_view {
            self.validate_view(view, false)
                .map_err(|e| invalid(&e.to_string()))?;
            for choice in view.boot.values().chain(view.actual.values()) {
                if let MemberChoice::Policy(reference) = choice {
                    if self.banks.get(&reference.bank).is_some_and(|lease| {
                        matches!(lease.phase, LeasePhase::Preparing | LeasePhase::Ready)
                    }) {
                        return Err(invalid(
                            "persisted front observation references an unpublished generation",
                        ));
                    }
                }
            }
            if view.boot.len() != self.layout.upstream_ids.len()
                || view.actual.len() != self.layout.upstream_ids.len()
            {
                return Err(invalid(
                    "persisted selector observations must retain explicit unknown members",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bank_resources_are_bounded_disjoint_and_have_distinct_api_endpoints() {
        let a: Vec<_> = (0..8).map(|i| Bank::A.policy_port(i).unwrap()).collect();
        let b: Vec<_> = (0..8).map(|i| Bank::B.policy_port(i).unwrap()).collect();
        assert_eq!(a, (2180..2188).collect::<Vec<_>>());
        assert_eq!(b, (2280..2288).collect::<Vec<_>>());
        assert!(a.iter().all(|p| !b.contains(p)));
        assert_eq!(Bank::A.policy_port(8), None);
        assert_eq!(Bank::B.policy_port(usize::MAX), None);
        assert_eq!(Bank::A.api(), "127.0.0.1:9093");
        assert_eq!(Bank::B.api(), "127.0.0.1:9094");
    }

    #[test]
    fn bank_member_tags_keep_the_canonical_supplier_uuid() {
        let id = Uuid::from_u128(1234);
        assert_eq!(Bank::A.member_tag(id), format!("resi-policy-bank-a-{id}"));
        assert_eq!(Bank::B.member_tag(id), format!("resi-policy-bank-b-{id}"));
        assert_ne!(Bank::A.member_tag(id), Bank::B.member_tag(id));
    }

    fn sha(byte: char) -> Sha256 {
        Sha256::new(byte.to_string().repeat(64)).unwrap()
    }
    fn ids() -> Vec<Uuid> {
        vec![Uuid::from_u128(1), Uuid::from_u128(2)]
    }
    fn layout() -> GenerationLayout {
        GenerationLayout {
            front_sha256: sha('a'),
            kernel_sha256: sha('b'),
            upstream_ids: ids(),
        }
    }
    fn ledger() -> GenerationLedger {
        GenerationLedger::new(layout()).unwrap()
    }
    fn ready_evidence(binding: &GenerationBinding) -> PreparedEvidence {
        PreparedEvidence {
            binding: binding.clone(),
            kernel_version: "1.14.2".into(),
            checked: true,
            api_upstreams: ids().into_iter().collect(),
            probed_upstreams: ids().into_iter().collect(),
        }
    }
    fn choices(binding: &GenerationBinding) -> BTreeMap<String, MemberChoice> {
        [
            (
                "resi-1".into(),
                MemberChoice::Policy(PolicyRef {
                    generation: binding.generation,
                    bank: binding.bank,
                    upstream_id: Uuid::from_u128(1),
                }),
            ),
            (
                "resi-2".into(),
                MemberChoice::Policy(PolicyRef {
                    generation: binding.generation,
                    bank: binding.bank,
                    upstream_id: Uuid::from_u128(2),
                }),
            ),
        ]
        .into_iter()
        .collect()
    }
    fn direct() -> BTreeMap<String, MemberChoice> {
        [
            ("resi-1".into(), MemberChoice::Direct),
            ("resi-2".into(), MemberChoice::Direct),
        ]
        .into_iter()
        .collect()
    }
    fn view(
        boot: BTreeMap<String, MemberChoice>,
        actual: BTreeMap<String, MemberChoice>,
    ) -> FrontView {
        FrontView {
            front_sha256: sha('a'),
            instance: Uuid::from_u128(90),
            selector_epoch: 100,
            boot,
            actual,
        }
    }
    fn activate(l: &mut GenerationLedger, config: char) -> GenerationBinding {
        let b = l.reserve(sha(config)).unwrap();
        l.mark_ready(b.bank, &ready_evidence(&b)).unwrap();
        l.begin_publish(b.bank).unwrap();
        let mut observed = view(choices(&b), choices(&b));
        observed.selector_epoch = b.generation * 5;
        assert_eq!(
            l.observe_publish(b.bank, &observed).unwrap(),
            PublishProgress::Active
        );
        b
    }
    fn active_pair() -> (GenerationLedger, GenerationBinding, GenerationBinding) {
        let mut l = ledger();
        let old = activate(&mut l, 'c');
        let new = activate(&mut l, 'd');
        (l, old, new)
    }
    fn reclaim(old: &GenerationBinding, new: &GenerationBinding) -> ReclaimEvidence {
        let selectors = view(choices(new), choices(new));
        ReclaimEvidence {
            dial_barrier: Some(FrontDialBarrier {
                generation: old.generation,
                front_instance: selectors.instance,
                selector_epoch: selectors.selector_epoch,
            }),
            selectors,
            flows: FlowObservation {
                generation: old.generation,
                tcp: Some(0),
                udp: Some(0),
                handshakes: Some(0),
            },
        }
    }
    fn stopped(binding: &GenerationBinding) -> StopEvidence {
        StopEvidence {
            binding: binding.clone(),
            command_succeeded: true,
            observed_inactive: true,
        }
    }
    fn unchanged<T: std::fmt::Debug>(
        l: &mut GenerationLedger,
        f: impl FnOnce(&mut GenerationLedger) -> Result<T, ProtocolError>,
        error: ProtocolError,
    ) {
        let before = l.to_json().unwrap();
        assert_eq!(f(l).unwrap_err(), error);
        assert_eq!(l.to_json().unwrap(), before);
    }

    #[test]
    fn sha_rejects_truncation_uppercase_nonhex_and_deserialization_bypass() {
        for s in [
            "a".repeat(63),
            "A".repeat(64),
            "g".repeat(64),
            "a".repeat(65),
        ] {
            assert!(Sha256::new(&s).is_err());
            assert!(serde_json::from_value::<Sha256>(serde_json::json!(s)).is_err());
        }
        assert_eq!(
            Sha256::new("abcdef0123456789".repeat(4)).unwrap().as_str(),
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
    }

    #[test]
    fn invalid_supplier_layout_never_creates_a_ledger() {
        for bad in [
            vec![],
            vec![Uuid::nil()],
            vec![Uuid::from_u128(1); 2],
            (1..=9).map(Uuid::from_u128).collect(),
        ] {
            let mut v = layout();
            v.upstream_ids = bad;
            assert!(GenerationLedger::new(v).is_err());
        }
        assert!(GenerationLedger::new(layout()).is_ok());
    }

    #[test]
    fn reserve_binds_all_content_and_starts_preparing() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        assert_eq!(b.generation, 1);
        assert_eq!(b.bank, Bank::A);
        assert_eq!(b.config_sha256, sha('c'));
        assert_eq!(b.kernel_sha256, sha('b'));
        assert_eq!(b.front_sha256, sha('a'));
        assert_eq!(b.upstream_ids, ids());
        assert_eq!(l.lease(Bank::A).unwrap().phase(), LeasePhase::Preparing);
    }

    #[test]
    fn concurrent_candidate_is_rejected_without_overwriting_the_first() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        unchanged(
            &mut l,
            |l| l.reserve(sha('d')),
            ProtocolError::CandidateInProgress,
        );
        assert_eq!(l.lease(Bank::A).unwrap().binding(), &b);
        assert!(l.lease(Bank::B).is_none());
    }

    #[test]
    fn two_leased_banks_force_wait_for_drain() {
        let (mut l, old, new) = active_pair();
        unchanged(&mut l, |l| l.reserve(sha('e')), ProtocolError::NeedDrain);
        assert_eq!(l.lease(old.bank).unwrap().phase(), LeasePhase::Draining);
        assert_eq!(l.lease(new.bank).unwrap().phase(), LeasePhase::Active);
    }

    #[test]
    fn changed_kernel_front_or_supplier_order_require_maintenance() {
        let l = ledger();
        assert_eq!(l.check_layout(&layout()), Ok(()));
        let mut front = layout();
        front.front_sha256 = sha('c');
        let mut kernel = layout();
        kernel.kernel_sha256 = sha('c');
        let mut order = layout();
        order.upstream_ids.reverse();
        let mut replaced = layout();
        replaced.upstream_ids[0] = Uuid::from_u128(3);
        for changed in [front, kernel, order, replaced] {
            assert_eq!(
                l.check_layout(&changed),
                Err(ProtocolError::MaintenanceRequired)
            );
        }
    }

    #[test]
    fn ready_requires_exact_binding_check_api_and_every_supplier_probe() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        let valid = ready_evidence(&b);
        let mut wrong_config = valid.clone();
        wrong_config.binding.config_sha256 = sha('d');
        let mut wrong_version = valid.clone();
        wrong_version.kernel_version = "1.14.1".into();
        let mut unchecked = valid.clone();
        unchecked.checked = false;
        let mut missing_probe = valid.clone();
        missing_probe.probed_upstreams.remove(&Uuid::from_u128(2));
        let mut extra_api = valid.clone();
        extra_api.api_upstreams.insert(Uuid::from_u128(3));
        for bad in [
            wrong_config,
            wrong_version,
            unchecked,
            missing_probe,
            extra_api,
        ] {
            unchanged(
                &mut l,
                |l| l.mark_ready(Bank::A, &bad),
                ProtocolError::EvidenceMismatch,
            );
        }
        l.mark_ready(Bank::A, &valid).unwrap();
        assert_eq!(l.lease(Bank::A).unwrap().phase(), LeasePhase::Ready);
    }

    #[test]
    fn prepare_must_finish_before_any_publish_intent() {
        let mut l = ledger();
        l.reserve(sha('c')).unwrap();
        unchanged(
            &mut l,
            |l| l.begin_publish(Bank::A),
            ProtocolError::WrongPhase,
        );
    }

    #[test]
    fn partial_publish_keeps_both_generations_and_observes_each_member() {
        let mut l = ledger();
        let old = activate(&mut l, 'c');
        let new = l.reserve(sha('d')).unwrap();
        l.mark_ready(new.bank, &ready_evidence(&new)).unwrap();
        l.begin_publish(new.bank).unwrap();
        let mut actual = choices(&old);
        actual.insert("resi-1".into(), choices(&new)["resi-1"].clone());
        let mixed = view(choices(&new), actual);
        assert_eq!(
            l.observe_publish(new.bank, &mixed).unwrap(),
            PublishProgress::Partial
        );
        assert_eq!(l.lease(old.bank).unwrap().phase(), LeasePhase::Active);
        assert_eq!(l.lease(new.bank).unwrap().phase(), LeasePhase::Publishing);
        let restored = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(restored, l);
        assert_eq!(
            l.authorize_stop(new.bank, &reclaim(&new, &old)),
            Err(ProtocolError::WrongPhase)
        );
    }

    #[test]
    fn missing_or_unknown_actual_member_cannot_finish_publish() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        l.mark_ready(b.bank, &ready_evidence(&b)).unwrap();
        l.begin_publish(b.bank).unwrap();
        let mut missing = view(choices(&b), choices(&b));
        missing.actual.remove("resi-2");
        assert_eq!(
            l.observe_publish(b.bank, &missing).unwrap(),
            PublishProgress::Partial
        );
        let mut unknown = view(choices(&b), choices(&b));
        unknown
            .actual
            .insert("resi-2".into(), MemberChoice::Unknown);
        assert_eq!(
            l.observe_publish(b.bank, &unknown).unwrap(),
            PublishProgress::Partial
        );
        assert_eq!(l.lease(b.bank).unwrap().phase(), LeasePhase::Publishing);
    }

    #[test]
    fn full_publish_drains_old_only_after_every_actual_member_changed() {
        let (l, old, new) = active_pair();
        assert_eq!(l.lease(old.bank).unwrap().phase(), LeasePhase::Draining);
        assert_eq!(l.lease(new.bank).unwrap().phase(), LeasePhase::Active);
    }

    #[test]
    fn old_boot_reference_blocks_reclamation_even_after_actual_switch() {
        let (l, old, new) = active_pair();
        let mut e = reclaim(&old, &new);
        e.selectors.boot = choices(&old);
        assert_eq!(
            l.authorize_stop(old.bank, &e),
            Err(ProtocolError::StillReferenced)
        );
    }

    #[test]
    fn old_actual_reference_blocks_reclamation_even_after_boot_switch() {
        let (l, old, new) = active_pair();
        let mut e = reclaim(&old, &new);
        e.selectors.actual = choices(&old);
        assert_eq!(
            l.authorize_stop(old.bank, &e),
            Err(ProtocolError::StillReferenced)
        );
    }

    #[test]
    fn missing_unknown_wrong_generation_or_wrong_supplier_observations_block_gc() {
        let (l, old, new) = active_pair();
        let good = reclaim(&old, &new);
        let mut missing = good.clone();
        missing.selectors.boot.remove("resi-2");
        let mut unknown = good.clone();
        unknown
            .selectors
            .actual
            .insert("resi-1".into(), MemberChoice::Unknown);
        for bad in [missing, unknown] {
            assert_eq!(
                l.authorize_stop(old.bank, &bad),
                Err(ProtocolError::UnknownObservation)
            );
        }
        let mut wrong = good;
        wrong.selectors.actual.insert(
            "resi-1".into(),
            MemberChoice::Policy(PolicyRef {
                generation: 99,
                bank: Bank::B,
                upstream_id: Uuid::from_u128(1),
            }),
        );
        assert_eq!(
            l.authorize_stop(old.bank, &wrong),
            Err(ProtocolError::EvidenceMismatch)
        );
    }

    #[test]
    fn tcp_udp_and_handshake_counts_must_each_be_known_zero() {
        let (l, old, new) = active_pair();
        let good = reclaim(&old, &new);
        for counts in [
            (None, Some(0), Some(0)),
            (Some(0), None, Some(0)),
            (Some(0), Some(0), None),
            (Some(1), Some(0), Some(0)),
            (Some(0), Some(1), Some(0)),
            (Some(0), Some(0), Some(1)),
        ] {
            let mut e = good.clone();
            e.flows.tcp = counts.0;
            e.flows.udp = counts.1;
            e.flows.handshakes = counts.2;
            assert_eq!(
                l.authorize_stop(old.bank, &e),
                Err(ProtocolError::NotDrained)
            );
        }
    }

    #[test]
    fn zero_api_connections_without_a_real_dial_barrier_never_authorize_stop() {
        let (l, old, new) = active_pair();
        let mut e = reclaim(&old, &new);
        e.dial_barrier = None;
        assert_eq!(
            l.authorize_stop(old.bank, &e),
            Err(ProtocolError::BarrierMissing)
        );
    }

    #[test]
    fn barrier_must_bind_generation_front_instance_and_selector_epoch() {
        let (l, old, new) = active_pair();
        let good = reclaim(&old, &new);
        let mut generation = good.clone();
        generation.dial_barrier.as_mut().unwrap().generation = new.generation;
        let mut instance = good.clone();
        instance.dial_barrier.as_mut().unwrap().front_instance = Uuid::from_u128(91);
        let mut epoch = good;
        epoch.dial_barrier.as_mut().unwrap().selector_epoch = 4;
        for bad in [generation, instance, epoch] {
            assert_eq!(
                l.authorize_stop(old.bank, &bad),
                Err(ProtocolError::BarrierMissing)
            );
        }
    }

    #[test]
    fn failed_stop_or_still_active_backend_keeps_the_old_bank_leased() {
        let (mut l, old, new) = active_pair();
        let a = l.authorize_stop(old.bank, &reclaim(&old, &new)).unwrap();
        let mut failure = stopped(&old);
        failure.command_succeeded = false;
        let mut active = stopped(&old);
        active.observed_inactive = false;
        for bad in [failure, active] {
            unchanged(
                &mut l,
                |l| l.mark_stopped(old.bank, &a, &bad),
                ProtocolError::StopFailed,
            );
        }
        assert_eq!(l.lease(old.bank).unwrap().phase(), LeasePhase::Draining);
    }

    #[test]
    fn verified_stop_and_free_allow_bank_reuse_with_a_new_generation() {
        let (mut l, old, new) = active_pair();
        let e = reclaim(&old, &new);
        let a = l.authorize_stop(old.bank, &e).unwrap();
        l.mark_stopped(old.bank, &a, &stopped(&old)).unwrap();
        assert_eq!(l.lease(old.bank).unwrap().phase(), LeasePhase::Stopped);
        l.free_stopped(old.bank, &e.selectors).unwrap();
        let replacement = l.reserve(sha('e')).unwrap();
        assert_eq!(replacement.bank, Bank::A);
        assert_eq!(replacement.generation, 3);
        assert_eq!(l.lease(new.bank).unwrap().binding(), &new);
    }

    #[test]
    fn a_reappearing_boot_reference_prevents_freeing_even_a_stopped_bank() {
        let (mut l, old, new) = active_pair();
        let e = reclaim(&old, &new);
        let a = l.authorize_stop(old.bank, &e).unwrap();
        l.mark_stopped(old.bank, &a, &stopped(&old)).unwrap();
        let mut reappeared = view(choices(&old), choices(&new));
        reappeared.selector_epoch = 101;
        unchanged(
            &mut l,
            |l| l.free_stopped(old.bank, &reappeared),
            ProtocolError::StillReferenced,
        );
    }

    #[test]
    fn stale_authorization_cannot_be_consumed_after_another_ledger_change() {
        let (mut l, old, new) = active_pair();
        let e = reclaim(&old, &new);
        let a = l.authorize_stop(old.bank, &e).unwrap();
        l.mark_stopped(old.bank, &a, &stopped(&old)).unwrap();
        unchanged(
            &mut l,
            |l| l.mark_stopped(old.bank, &a, &stopped(&old)),
            ProtocolError::StaleAuthorization,
        );
    }

    #[test]
    fn prepare_failure_cleans_only_fully_unreferenced_confirmed_inactive_candidates() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        let mut unknown = view(direct(), direct());
        unknown.actual.remove("resi-2");
        unchanged(
            &mut l,
            |l| l.discard_candidate(b.bank, &unknown, &stopped(&b)),
            ProtocolError::UnknownObservation,
        );
        unchanged(
            &mut l,
            |l| l.discard_candidate(b.bank, &view(choices(&b), direct()), &stopped(&b)),
            ProtocolError::StillReferenced,
        );
        let clear = view(direct(), direct());
        let mut failed = stopped(&b);
        failed.command_succeeded = false;
        unchanged(
            &mut l,
            |l| l.discard_candidate(b.bank, &clear, &failed),
            ProtocolError::StopFailed,
        );
        l.discard_candidate(b.bank, &clear, &stopped(&b)).unwrap();
        assert_eq!(l.lease(b.bank).unwrap().phase(), LeasePhase::Stopped);
        l.free_stopped(b.bank, &clear).unwrap();
        assert!(l.lease(b.bank).is_none());
    }

    #[test]
    fn partial_publish_cannot_use_prepare_failure_cleanup() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        l.mark_ready(b.bank, &ready_evidence(&b)).unwrap();
        l.begin_publish(b.bank).unwrap();
        unchanged(
            &mut l,
            |l| l.discard_candidate(b.bank, &view(direct(), direct()), &stopped(&b)),
            ProtocolError::WrongPhase,
        );
    }

    #[test]
    fn crash_snapshots_roundtrip_every_transaction_boundary_without_guessing() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        l = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(l.lease(b.bank).unwrap().phase(), LeasePhase::Preparing);
        l.mark_ready(b.bank, &ready_evidence(&b)).unwrap();
        l = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(l.lease(b.bank).unwrap().phase(), LeasePhase::Ready);
        l.begin_publish(b.bank).unwrap();
        l = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(l.lease(b.bank).unwrap().phase(), LeasePhase::Publishing);
        let mut published = view(choices(&b), choices(&b));
        published.selector_epoch = 5;
        l.observe_publish(b.bank, &published).unwrap();
        l = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        let n = activate(&mut l, 'd');
        l = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        let e = reclaim(&b, &n);
        let a = l.authorize_stop(b.bank, &e).unwrap();
        l.mark_stopped(b.bank, &a, &stopped(&b)).unwrap();
        l = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        l.free_stopped(b.bank, &e.selectors).unwrap();
        let b3 = l.reserve(sha('e')).unwrap();
        assert_eq!(b3.generation, 3);
    }

    #[test]
    fn corrupted_snapshot_and_illegal_phase_bindings_are_rejected_without_reset() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        let base = serde_json::to_value(&l).unwrap();
        let mut variants = vec![];
        let mut unknown = base.clone();
        unknown["unexpected"] = serde_json::json!(true);
        variants.push(unknown);
        let mut sha = base.clone();
        sha["banks"]["a"]["binding"]["config_sha256"] = serde_json::json!("bad");
        variants.push(sha);
        let mut kernel = base.clone();
        kernel["banks"]["a"]["binding"]["kernel_sha256"] = serde_json::json!("e".repeat(64));
        variants.push(kernel);
        let mut zero = base.clone();
        zero["banks"]["a"]["binding"]["generation"] = serde_json::json!(0);
        variants.push(zero);
        let mut bank = base.clone();
        bank["banks"]["a"]["binding"]["bank"] = serde_json::json!("b");
        variants.push(bank);
        let mut phase = base.clone();
        phase["banks"]["a"]["phase"] = serde_json::json!("active");
        variants.push(phase);
        let mut nil = base.clone();
        nil["layout"]["upstream_ids"][0] = serde_json::json!(Uuid::nil());
        variants.push(nil);
        let mut schema = base.clone();
        schema["schema_version"] = serde_json::json!(2);
        variants.push(schema);
        for bad in variants {
            let bytes = serde_json::to_vec(&bad).unwrap();
            assert!(GenerationLedger::from_json(&bytes).is_err(), "{bad}");
            assert!(serde_json::from_slice::<GenerationLedger>(&bytes).is_err());
        }
        assert!(GenerationLedger::from_json(b"{").is_err());
        assert_eq!(l.lease(b.bank).unwrap().binding(), &b);
    }

    #[test]
    fn generation_and_uuid_log_binding_excludes_ready_draining_and_wrong_members() {
        let mut l = ledger();
        let old = activate(&mut l, 'c');
        let new = l.reserve(sha('d')).unwrap();
        l.mark_ready(new.bank, &ready_evidence(&new)).unwrap();
        assert!(l
            .current_policy_for_log(
                new.generation,
                Uuid::from_u128(1),
                &view(choices(&old), choices(&old))
            )
            .is_none());
        l.begin_publish(new.bank).unwrap();
        let mut mixed = choices(&old);
        mixed.insert("resi-1".into(), choices(&new)["resi-1"].clone());
        let mixed = view(choices(&new), mixed);
        l.observe_publish(new.bank, &mixed).unwrap();
        assert_eq!(
            l.current_policy_for_log(new.generation, Uuid::from_u128(1), &mixed),
            Some(&new)
        );
        assert!(l
            .current_policy_for_log(old.generation, Uuid::from_u128(1), &mixed)
            .is_none());
        assert_eq!(
            l.current_policy_for_log(old.generation, Uuid::from_u128(2), &mixed),
            Some(&old)
        );
        let mut final_view = view(choices(&new), choices(&new));
        final_view.selector_epoch = 101;
        l.observe_publish(new.bank, &final_view).unwrap();
        assert!(l
            .current_policy_for_log(old.generation, Uuid::from_u128(2), &final_view)
            .is_none());
        assert!(l
            .current_policy_for_log(new.generation, Uuid::from_u128(999), &final_view)
            .is_none());
        assert!(l
            .current_policy_for_log(99, Uuid::from_u128(1), &final_view)
            .is_none());
    }

    #[test]
    fn wrong_front_instance_or_config_binding_never_provides_current_policy() {
        let mut l = ledger();
        let b = activate(&mut l, 'c');
        let mut bad = view(choices(&b), choices(&b));
        bad.front_sha256 = sha('f');
        assert!(l
            .current_policy_for_log(b.generation, Uuid::from_u128(1), &bad)
            .is_none());
        bad.front_sha256 = sha('a');
        bad.instance = Uuid::nil();
        assert!(l
            .current_policy_for_log(b.generation, Uuid::from_u128(1), &bad)
            .is_none());
    }

    #[test]
    fn duplicate_bank_member_and_nested_binding_keys_are_rejected_as_ambiguous() {
        let mut l = ledger();
        let b = activate(&mut l, 'c');
        let json = String::from_utf8(l.to_json().unwrap()).unwrap();
        let duplicate_top = json.replacen(
            "\"schema_version\": 1",
            "\"schema_version\": 1, \"schema_version\": 1",
            1,
        );
        let duplicate_bank = format!(
            "{{\"a\":{},\"a\":{}}}",
            serde_json::to_string(l.lease(b.bank).unwrap()).unwrap(),
            serde_json::to_string(l.lease(b.bank).unwrap()).unwrap()
        );
        let mut value = serde_json::to_value(&l).unwrap();
        value["banks"] = serde_json::json!("__DUP_BANK__");
        let duplicate_bank = serde_json::to_string(&value)
            .unwrap()
            .replace("\"__DUP_BANK__\"", &duplicate_bank);
        let duplicate_member = json.replacen(
            "\"resi-1\": {",
            "\"resi-1\": {\"kind\":\"direct\"}, \"resi-1\": {",
            1,
        );
        let duplicate_generation = json.replacen(
            "\"generation\": 1",
            "\"generation\": 1, \"generation\": 1",
            1,
        );
        for bad in [
            duplicate_top,
            duplicate_bank,
            duplicate_member,
            duplicate_generation,
        ] {
            assert!(
                GenerationLedger::from_json(bad.as_bytes()).is_err(),
                "{bad}"
            );
            assert!(serde_json::from_str::<GenerationLedger>(&bad).is_err());
        }
    }

    #[test]
    fn snapshots_reject_noncanonical_uuids_and_next_counter_reuse() {
        let mut l = ledger();
        activate(&mut l, 'c');
        let base = serde_json::to_value(&l).unwrap();
        let mut simple = base.clone();
        simple["layout"]["upstream_ids"][0] =
            serde_json::json!(Uuid::from_u128(1).simple().to_string());
        let mut malformed = base.clone();
        malformed["banks"]["a"]["binding"]["upstream_ids"][0] = serde_json::json!("not-a-uuid");
        let mut reuse = base;
        reuse["next_generation"] = serde_json::json!(1);
        for bad in [simple, malformed, reuse] {
            assert!(GenerationLedger::from_json(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
    }

    #[test]
    fn evidence_error_never_partly_applies_selector_observations() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        l.mark_ready(b.bank, &ready_evidence(&b)).unwrap();
        l.begin_publish(b.bank).unwrap();
        let mut swapped = choices(&b);
        swapped.insert(
            "resi-1".into(),
            MemberChoice::Policy(PolicyRef {
                generation: b.generation,
                bank: b.bank,
                upstream_id: Uuid::from_u128(2),
            }),
        );
        unchanged(
            &mut l,
            |l| l.observe_publish(b.bank, &view(choices(&b), swapped)),
            ProtocolError::EvidenceMismatch,
        );
        assert_eq!(l.lease(b.bank).unwrap().phase(), LeasePhase::Publishing);
    }

    #[test]
    fn snapshot_rejects_orphan_drain_and_generations_older_than_the_current_active() {
        let (l, old, new) = active_pair();
        let base = serde_json::to_value(&l).unwrap();
        let mut orphan = base.clone();
        orphan["banks"].as_object_mut().unwrap().remove("b");
        orphan["front_view"]["boot"] = serde_json::to_value(choices(&old)).unwrap();
        orphan["front_view"]["actual"] = serde_json::to_value(choices(&old)).unwrap();
        let mut backwards = base.clone();
        backwards["next_generation"] = serde_json::json!(101);
        for (bank, generation) in [("a", 100), ("b", 99)] {
            backwards["banks"][bank]["binding"]["generation"] = serde_json::json!(generation);
            for member in ["resi-1", "resi-2"] {
                backwards["banks"][bank]["publish"]["members"][member]["generation"] =
                    serde_json::json!(generation);
            }
        }
        for source in ["boot", "actual"] {
            for member in ["resi-1", "resi-2"] {
                backwards["front_view"][source][member]["generation"] = serde_json::json!(99);
            }
        }
        let mut older_candidate = base;
        older_candidate["banks"]["a"]["phase"] = serde_json::json!("ready");
        older_candidate["banks"]["a"]["publish"] = serde_json::Value::Null;
        for bad in [orphan, backwards, older_candidate] {
            assert!(
                GenerationLedger::from_json(&serde_json::to_vec(&bad).unwrap()).is_err(),
                "{bad}"
            );
        }
        assert_eq!(new.generation, 2);
    }

    #[test]
    fn stopped_snapshots_require_complete_known_observations_without_own_references() {
        let mut l = ledger();
        l.reserve(sha('c')).unwrap();
        let mut missing = serde_json::to_value(&l).unwrap();
        missing["banks"]["a"]["phase"] = serde_json::json!("stopped");
        let mut l = ledger();
        let b = activate(&mut l, 'c');
        let mut referenced = serde_json::to_value(&l).unwrap();
        referenced["banks"]["a"]["phase"] = serde_json::json!("stopped");
        let mut unknown = referenced.clone();
        unknown["front_view"]["actual"] = serde_json::to_value(direct()).unwrap();
        unknown["front_view"]["boot"] = serde_json::to_value(direct()).unwrap();
        unknown["front_view"]["boot"]["resi-1"] = serde_json::json!({"kind":"unknown"});
        for bad in [missing, referenced, unknown] {
            assert!(
                GenerationLedger::from_json(&serde_json::to_vec(&bad).unwrap()).is_err(),
                "{bad}"
            );
        }
        assert_eq!(b.generation, 1);
    }

    #[test]
    fn a_typed_reservation_can_compile_and_hash_the_exact_backend_before_leasing() {
        use sha2::Digest;
        let mut l = ledger();
        let identity = l.reservation_identity().unwrap();
        assert_eq!(
            identity,
            ReservationIdentity {
                generation: 1,
                bank: Bank::A
            }
        );
        assert!(l.lease(Bank::A).is_none());
        let group: crate::model::ResidentialGroup = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "upstreams": [
                {"id": "00000000-0000-0000-0000-000000000001", "name": "fixture-a", "kind": "socks5", "host": "192.0.2.1", "port": 1080},
                {"id": "00000000-0000-0000-0000-000000000002", "name": "fixture-b", "kind": "socks5", "host": "192.0.2.2", "port": 1080}
            ]
        })).unwrap();
        let config = crate::render::relay::generation::backend_config(
            &group,
            &crate::render::relay::generation::BackendOpts {
                bank: identity.bank,
                generation: identity.generation,
                server_ip: None,
            },
        )
        .unwrap();
        let bytes = serde_json::to_vec(&config).unwrap();
        let digest = Sha256::new(hex::encode(sha2::Sha256::digest(&bytes))).unwrap();
        let binding = l.reserve_expected(identity, digest.clone()).unwrap();
        assert_eq!(binding.generation, 1);
        assert_eq!(binding.bank, Bank::A);
        assert_eq!(binding.config_sha256, digest);
        assert!(config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["tag"] == "resi-egress-g1-00000000-0000-0000-0000-000000000001"));
    }

    #[test]
    fn stale_reservation_identity_never_binds_an_old_compiled_config_to_a_reused_bank() {
        let mut l = ledger();
        let first = l.reservation_identity().unwrap();
        let b = l.reserve_expected(first, sha('c')).unwrap();
        let clear = view(direct(), direct());
        l.discard_candidate(b.bank, &clear, &stopped(&b)).unwrap();
        l.free_stopped(b.bank, &clear).unwrap();
        assert_eq!(
            l.reservation_identity().unwrap(),
            ReservationIdentity {
                generation: 2,
                bank: Bank::A
            }
        );
        unchanged(
            &mut l,
            |l| l.reserve_expected(first, sha('d')),
            ProtocolError::StaleReservation,
        );
    }

    #[test]
    fn same_epoch_conflicting_known_selection_cannot_reverse_learning_or_publication() {
        let mut l = ledger();
        let old = activate(&mut l, 'c');
        let new = l.reserve(sha('d')).unwrap();
        l.mark_ready(new.bank, &ready_evidence(&new)).unwrap();
        l.begin_publish(new.bank).unwrap();
        let mut actual = choices(&old);
        actual.insert("resi-1".into(), choices(&new)["resi-1"].clone());
        let mut partial = view(choices(&new), actual);
        partial.selector_epoch = 6;
        l.observe_publish(new.bank, &partial).unwrap();
        let mut conflicting = view(choices(&old), choices(&old));
        conflicting.selector_epoch = 6;
        assert!(l
            .current_policy_for_log(old.generation, Uuid::from_u128(1), &conflicting)
            .is_none());
        unchanged(
            &mut l,
            |l| l.observe_publish(new.bank, &conflicting),
            ProtocolError::EvidenceMismatch,
        );
        assert_eq!(
            l.current_policy_for_log(new.generation, Uuid::from_u128(1), &partial),
            Some(&new)
        );
    }

    #[test]
    fn same_epoch_unknown_members_can_be_filled_without_changing_any_known_selection() {
        let mut l = ledger();
        let b = l.reserve(sha('c')).unwrap();
        l.mark_ready(b.bank, &ready_evidence(&b)).unwrap();
        l.begin_publish(b.bank).unwrap();
        let mut partial = view(choices(&b), choices(&b));
        partial.selector_epoch = 6;
        partial.boot.insert("resi-2".into(), MemberChoice::Unknown);
        partial
            .actual
            .insert("resi-2".into(), MemberChoice::Unknown);
        assert_eq!(
            l.observe_publish(b.bank, &partial).unwrap(),
            PublishProgress::Partial
        );
        let mut complete = view(choices(&b), choices(&b));
        complete.selector_epoch = 6;
        assert_eq!(
            l.observe_publish(b.bank, &complete).unwrap(),
            PublishProgress::Active
        );
        let restored = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(restored.lease(b.bank).unwrap().phase(), LeasePhase::Active);
    }

    #[test]
    fn stopped_lease_must_be_freed_before_another_candidate_can_persist_unknown_publication() {
        let mut l = ledger();
        let old = l.reserve(sha('c')).unwrap();
        let clear = view(direct(), direct());
        l.discard_candidate(old.bank, &clear, &stopped(&old))
            .unwrap();
        assert_eq!(l.lease(Bank::A).unwrap().phase(), LeasePhase::Stopped);
        assert_eq!(l.reservation_identity(), Err(ProtocolError::NeedDrain));
        unchanged(&mut l, |l| l.reserve(sha('d')), ProtocolError::NeedDrain);
        unchanged(
            &mut l,
            |l| {
                l.reserve_expected(
                    ReservationIdentity {
                        generation: 2,
                        bank: Bank::B,
                    },
                    sha('d'),
                )
            },
            ProtocolError::NeedDrain,
        );
        l.free_stopped(old.bank, &clear).unwrap();
        let identity = l.reservation_identity().unwrap();
        assert_eq!(
            identity,
            ReservationIdentity {
                generation: 2,
                bank: Bank::A
            }
        );
        let candidate = l.reserve_expected(identity, sha('d')).unwrap();
        l.mark_ready(candidate.bank, &ready_evidence(&candidate))
            .unwrap();
        l.begin_publish(candidate.bank).unwrap();
        let mut unknown = view(choices(&candidate), BTreeMap::new());
        unknown.selector_epoch = 101;
        assert_eq!(
            l.observe_publish(candidate.bank, &unknown).unwrap(),
            PublishProgress::Partial
        );
        assert_eq!(
            l.lease(candidate.bank).unwrap().phase(),
            LeasePhase::Publishing
        );
        let restored = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(restored, l);
        assert_eq!(
            restored.front_view.as_ref().unwrap().actual["resi-1"],
            MemberChoice::Unknown
        );
        assert_eq!(
            restored.front_view.as_ref().unwrap().actual["resi-2"],
            MemberChoice::Unknown
        );
    }

    #[test]
    fn snapshots_reject_stopped_candidate_combinations_and_impossible_stop_history() {
        let mut l = ledger();
        let old = l.reserve(sha('c')).unwrap();
        let clear = view(direct(), direct());
        l.discard_candidate(old.bank, &clear, &stopped(&old))
            .unwrap();
        let base = serde_json::to_value(&l).unwrap();
        let mut variants = Vec::new();
        let b2 = GenerationBinding {
            generation: 2,
            bank: Bank::B,
            config_sha256: sha('d'),
            kernel_sha256: sha('b'),
            front_sha256: sha('a'),
            upstream_ids: ids(),
        };
        for phase in ["preparing", "ready", "publishing"] {
            let mut bad = base.clone();
            bad["next_generation"] = serde_json::json!(3);
            let publish = if phase == "publishing" {
                serde_json::json!({"members": choices(&b2)})
            } else {
                serde_json::Value::Null
            };
            bad["banks"]["b"] =
                serde_json::json!({"binding": b2, "phase": phase, "publish": publish});
            variants.push((format!("Stopped + {phase}"), bad));
        }
        let mut two_stopped = base.clone();
        two_stopped["next_generation"] = serde_json::json!(3);
        two_stopped["banks"]["b"] =
            serde_json::json!({"binding": b2, "phase": "stopped", "publish": null});
        variants.push(("two Stopped".into(), two_stopped));
        let mut published_alone = base;
        published_alone["banks"]["a"]["publish"] = serde_json::json!({"members": choices(&old)});
        variants.push(("published Stopped without Active".into(), published_alone));
        let (pair, old, _active) = active_pair();
        let pair = serde_json::to_value(&pair).unwrap();
        let mut a3 = old.clone();
        a3.generation = 3;
        let mut newer_published = pair.clone();
        newer_published["next_generation"] = serde_json::json!(4);
        newer_published["banks"]["a"] = serde_json::json!({"binding": a3, "phase": "stopped", "publish": {"members": choices(&a3)}});
        variants.push((
            "published Stopped newer than Active".into(),
            newer_published,
        ));
        let mut older_failed = pair;
        older_failed["banks"]["a"]["phase"] = serde_json::json!("stopped");
        older_failed["banks"]["a"]["publish"] = serde_json::Value::Null;
        variants.push((
            "failed candidate Stopped older than Active".into(),
            older_failed,
        ));
        let mut unpublished = ledger();
        let candidate = unpublished.reserve(sha('c')).unwrap();
        let unpublished = serde_json::to_value(&unpublished).unwrap();
        for phase in ["preparing", "ready"] {
            let mut bad = unpublished.clone();
            bad["banks"]["a"]["phase"] = serde_json::json!(phase);
            bad["front_view"] =
                serde_json::to_value(view(choices(&candidate), choices(&candidate))).unwrap();
            variants.push((format!("persistent front references {phase}"), bad));
        }
        let mut accepted = Vec::new();
        for (name, bad) in variants {
            let bytes = serde_json::to_vec(&bad).unwrap();
            if GenerationLedger::from_json(&bytes).is_ok()
                || serde_json::from_slice::<GenerationLedger>(&bytes).is_ok()
            {
                accepted.push(name);
            }
        }
        assert!(
            accepted.is_empty(),
            "impossible lease combinations accepted: {accepted:?}"
        );
    }

    #[test]
    fn newer_failed_candidate_can_stop_and_roundtrip_while_the_old_generation_stays_active() {
        let mut l = ledger();
        let active = activate(&mut l, 'c');
        let candidate = l.reserve(sha('d')).unwrap();
        l.mark_ready(candidate.bank, &ready_evidence(&candidate))
            .unwrap();
        let unchanged_front = view(choices(&active), choices(&active));
        l.discard_candidate(candidate.bank, &unchanged_front, &stopped(&candidate))
            .unwrap();
        let mut restored = GenerationLedger::from_json(&l.to_json().unwrap()).unwrap();
        assert_eq!(
            restored.lease(active.bank).unwrap().phase(),
            LeasePhase::Active
        );
        assert_eq!(
            restored.lease(candidate.bank).unwrap().phase(),
            LeasePhase::Stopped
        );
        assert_eq!(
            restored.reservation_identity(),
            Err(ProtocolError::NeedDrain)
        );
        restored
            .free_stopped(candidate.bank, &unchanged_front)
            .unwrap();
        assert_eq!(
            restored.reservation_identity().unwrap(),
            ReservationIdentity {
                generation: 3,
                bank: Bank::B
            }
        );
        assert_eq!(restored.lease(active.bank).unwrap().binding(), &active);
    }
}
