//! 住宅 relay 的实际出口策略端点：端口与 UUID 标签的唯一来源。
//!
//! selector 成员仍叫 `resi-N`，但它只连接回环 policy 入站。策略入站和真实供应商
//! 出站使用稳定 UUID；运行时日志归因不再靠可能已重新排序的池下标。
use crate::model::ResidentialGroup;
use crate::slots::MAX_SLOTS;
use uuid::Uuid;

/// 实际出口策略 SOCKS 入站的保留范围：`127.0.0.1:2180..2187`。
pub const POLICY_SOCKS_BASE: u16 = 2180;

/// 验证 relay 策略端点支持的池范围；由 state 读写边界在渲染之前调用。
/// 不截断上游、不修改凭据，也不把无效池静默变成直连。
pub fn validate_group(group: &ResidentialGroup) -> Result<(), String> {
    if group.upstreams.len() > usize::from(MAX_SLOTS) {
        return Err(format!(
            "住宅上游池包含 {} 条，最多支持 {MAX_SLOTS} 条；请先将池调整到支持范围，原配置未修改",
            group.upstreams.len()
        ));
    }
    Ok(())
}

/// 池位置对应的内部端口。越界返回 `None`，不得夹取造成多个上游共用一个端口。
pub fn policy_port(index: usize) -> Option<u16> {
    (index < usize::from(MAX_SLOTS)).then(|| POLICY_SOCKS_BASE + index as u16)
}

/// 每个上游的策略入站标签。
pub fn inbound_tag(id: Uuid) -> String {
    format!("resi-policy-{id}")
}

/// 每个真实供应商出站的标签；日志和运行态归因只认 UUID，不认池位置。
pub fn egress_tag(id: Uuid) -> String {
    format!("resi-egress-{id}")
}

/// 严格解析完整的真实出口标签，不接受尾随文本、非规范 UUID 或 selector 的位置标签。
pub fn upstream_from_egress_tag(tag: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(tag.strip_prefix("resi-egress-")?).ok()?;
    (egress_tag(id) == tag).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_ports_are_bounded_unique_and_separate_from_slot_ingress() {
        let ports: Vec<_> = (0..usize::from(MAX_SLOTS))
            .map(|i| policy_port(i).unwrap())
            .collect();
        assert_eq!(ports, (2180..2188).collect::<Vec<_>>());
        assert!(ports.iter().all(|p| !(crate::slots::RELAY_SOCKS_BASE
            ..crate::slots::RELAY_SOCKS_BASE + MAX_SLOTS)
            .contains(p)));
        assert_eq!(policy_port(usize::from(MAX_SLOTS)), None);
        assert_eq!(policy_port(usize::MAX), None);
    }

    #[test]
    fn raw_tags_roundtrip_and_reject_ambiguous_or_partial_identifiers() {
        let id = Uuid::from_u128(0xabcdef);
        assert_eq!(upstream_from_egress_tag(&egress_tag(id)), Some(id));
        for tag in [
            "resi-1".to_string(),
            inbound_tag(id),
            format!("{} extra", egress_tag(id)),
            egress_tag(id).to_uppercase(),
            format!("resi-egress-{}", id.simple()),
            "resi-egress-".into(),
        ] {
            assert_eq!(upstream_from_egress_tag(&tag), None, "{tag}");
        }
    }
}
