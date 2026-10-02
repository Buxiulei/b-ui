//! Authorization for a requested network path, shared by subscriptions and servers.
//!
//! A residential grant never implies a direct grant. A missing or ambiguous
//! residential binding is denied instead of being repaired by a routing fallback.
//! Allocation remains separate: temporarily disabled pools retain reserved credentials.

use crate::model::{Protocol, Residential, User, DEFAULT_GROUP};
use uuid::Uuid;

/// The path explicitly requested by an entry point or subscription node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedEgress {
    Direct,
    RequiredResidential,
}

/// The exact residential destination authorized for a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentialBinding {
    pub upstream_id: Uuid,
    pub slot_index: u16,
}

/// An authorized path; there is no implicit direct alternative to residential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizedEgress {
    Direct,
    Residential(ResidentialBinding),
}

/// Deterministic denial reasons containing no identity or credential values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EgressDeny {
    #[error("account disabled")]
    Disabled,
    #[error("account blocked")]
    AccountBlocked,
    #[error("protocol not granted")]
    ProtocolNotGranted,
    #[error("direct path not granted")]
    DirectNotGranted,
    #[error("residential path not granted")]
    ResidentialNotGranted,
    #[error("residential group missing")]
    MissingGroup,
    #[error("residential group unsupported")]
    UnsupportedGroup,
    #[error("residential pool disabled")]
    PoolDisabled,
    #[error("residential pool empty")]
    EmptyPool,
    #[error("residential slot unassigned")]
    MissingSlotBinding,
    #[error("residential slot missing")]
    MissingSlot,
    #[error("residential slot ambiguous")]
    AmbiguousSlot,
    #[error("residential slot index invalid")]
    InvalidSlotIndex,
    #[error("residential upstream missing")]
    MissingUpstream,
    #[error("residential upstream ambiguous")]
    AmbiguousUpstream,
    #[error("residential upstream invalid")]
    InvalidUpstream,
    #[error("residential HY2 credential missing")]
    MissingHy2Credential,
    #[error("residential HY2 credential invalid")]
    InvalidHy2Credential,
}

/// Authorize a requested path using the caller's existing quota/expiry decision.
///
/// `account_blocked` must come from the owning server's account policy. This pure
/// function does not read a second clock or invent a second accounting policy.
/// Only the default group is currently rendered into the shared relay, so any
/// other group is denied even if a similarly named desired-state group exists.
pub fn access_for(
    user: &User,
    residential: &Residential,
    protocol: Protocol,
    requested: RequestedEgress,
    account_blocked: bool,
) -> Result<AuthorizedEgress, EgressDeny> {
    if user.disabled {
        return Err(EgressDeny::Disabled);
    }
    if account_blocked {
        return Err(EgressDeny::AccountBlocked);
    }
    if !user.entitlements.protocols.contains(&protocol) {
        return Err(EgressDeny::ProtocolNotGranted);
    }
    if requested == RequestedEgress::Direct {
        return if user.entitlements.direct {
            Ok(AuthorizedEgress::Direct)
        } else {
            Err(EgressDeny::DirectNotGranted)
        };
    }
    let entitlement = user
        .entitlements
        .residential
        .as_ref()
        .ok_or(EgressDeny::ResidentialNotGranted)?;
    let group = residential
        .groups
        .get(&entitlement.group_id)
        .ok_or(EgressDeny::MissingGroup)?;
    if entitlement.group_id != DEFAULT_GROUP {
        return Err(EgressDeny::UnsupportedGroup);
    }
    if !group.enabled {
        return Err(EgressDeny::PoolDisabled);
    }
    if group.upstreams.is_empty() {
        return Err(EgressDeny::EmptyPool);
    }
    let upstream_id = entitlement.slot_id.ok_or(EgressDeny::MissingSlotBinding)?;
    let mut matches = residential
        .slots
        .iter()
        .filter(|slot| slot.upstream_id == upstream_id);
    let slot = matches.next().ok_or(EgressDeny::MissingSlot)?;
    if matches.next().is_some()
        || residential
            .slots
            .iter()
            .filter(|candidate| candidate.index == slot.index)
            .count()
            != 1
    {
        return Err(EgressDeny::AmbiguousSlot);
    }
    if slot.index >= crate::slots::MAX_SLOTS {
        return Err(EgressDeny::InvalidSlotIndex);
    }
    let mut matches = group
        .upstreams
        .iter()
        .filter(|upstream| upstream.id == upstream_id);
    let upstream = matches.next().ok_or(EgressDeny::MissingUpstream)?;
    if matches.next().is_some() {
        return Err(EgressDeny::AmbiguousUpstream);
    }
    if upstream.id.is_nil() || upstream.host.trim().is_empty() || upstream.port == 0 {
        return Err(EgressDeny::InvalidUpstream);
    }
    if protocol == Protocol::Hysteria2 {
        let id = user
            .credentials
            .hy2_resi_cred
            .as_ref()
            .ok_or(EgressDeny::MissingHy2Credential)?;
        let mut matches = residential
            .hy2_pool
            .creds
            .iter()
            .filter(|cred| &cred.id == id);
        let cred = matches.next().ok_or(EgressDeny::MissingHy2Credential)?;
        if matches.next().is_some()
            || cred.id.is_empty()
            || cred.name.is_empty()
            || cred.secret.is_empty()
            || residential
                .hy2_pool
                .creds
                .iter()
                .filter(|candidate| candidate.name == cred.name)
                .count()
                != 1
        {
            return Err(EgressDeny::InvalidHy2Credential);
        }
    }
    Ok(AuthorizedEgress::Residential(ResidentialBinding {
        upstream_id,
        slot_index: slot.index,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ReservedCred, Slot, Upstream, UpstreamKind};

    fn provisioned() -> (User, Residential) {
        let upstream_id = Uuid::from_u128(2);
        let user = serde_json::from_value(serde_json::json!({
            "user_id": "00000000-0000-0000-0000-000000000001",
            "username": "alice",
            "created_at": "2026-10-02T00:00:00Z",
            "credentials": {
                "hy2_password": "direct-secret",
                "vless_uuid": "00000000-0000-0000-0000-000000000011",
                "hy2_resi_cred": "r000"
            },
            "entitlements": {
                "protocols": ["hysteria2", "reality"], "direct": true,
                "residential": {"group_id": "default", "slot_id": upstream_id}
            }
        }))
        .unwrap();
        let mut residential = Residential::default();
        let group = residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        group.enabled = true;
        group.upstreams.push(Upstream {
            id: upstream_id,
            name: "declared-isp".into(),
            kind: UpstreamKind::Socks5,
            host: "isp.example.net".into(),
            port: 10007,
            username: "provider-user".into(),
            password: "provider-secret".into(),
            priority: 100,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        });
        residential.slots.push(Slot {
            index: 6,
            upstream_id,
        });
        residential.hy2_pool.creds.push(ReservedCred {
            id: "r000".into(),
            name: "r000".into(),
            secret: "reserved-secret".into(),
            released_at: None,
        });
        (user, residential)
    }

    #[test]
    fn authorization_keeps_the_exact_binding_and_explicit_direct_grant() {
        let (mut user, residential) = provisioned();
        let expected = Ok(AuthorizedEgress::Residential(ResidentialBinding {
            upstream_id: Uuid::from_u128(2),
            slot_index: 6,
        }));
        user.entitlements.direct = false;
        for protocol in [Protocol::Reality, Protocol::Hysteria2] {
            assert_eq!(
                access_for(
                    &user,
                    &residential,
                    protocol,
                    RequestedEgress::RequiredResidential,
                    false
                ),
                expected
            );
            assert_eq!(
                access_for(
                    &user,
                    &residential,
                    protocol,
                    RequestedEgress::Direct,
                    false
                ),
                Err(EgressDeny::DirectNotGranted)
            );
        }
        user.entitlements.direct = true;
        user.entitlements.residential = None;
        for protocol in [Protocol::Reality, Protocol::Hysteria2] {
            assert_eq!(
                access_for(
                    &user,
                    &residential,
                    protocol,
                    RequestedEgress::Direct,
                    false
                ),
                Ok(AuthorizedEgress::Direct)
            );
            assert_eq!(
                access_for(
                    &user,
                    &residential,
                    protocol,
                    RequestedEgress::RequiredResidential,
                    false
                ),
                Err(EgressDeny::ResidentialNotGranted)
            );
        }
    }

    #[test]
    fn account_decision_precedes_protocol_and_path_and_pool_failures() {
        let (mut user, mut residential) = provisioned();
        residential.groups.clear();
        user.entitlements.protocols.clear();
        user.disabled = true;
        assert_eq!(
            access_for(
                &user,
                &residential,
                Protocol::Hysteria2,
                RequestedEgress::RequiredResidential,
                true
            ),
            Err(EgressDeny::Disabled)
        );
        user.disabled = false;
        assert_eq!(
            access_for(
                &user,
                &residential,
                Protocol::Hysteria2,
                RequestedEgress::RequiredResidential,
                true
            ),
            Err(EgressDeny::AccountBlocked)
        );
        assert_eq!(
            access_for(
                &user,
                &residential,
                Protocol::Hysteria2,
                RequestedEgress::RequiredResidential,
                false
            ),
            Err(EgressDeny::ProtocolNotGranted)
        );
        user.entitlements.protocols.push(Protocol::Hysteria2);
        assert_eq!(
            access_for(
                &user,
                &residential,
                Protocol::Hysteria2,
                RequestedEgress::RequiredResidential,
                false
            ),
            Err(EgressDeny::MissingGroup)
        );
    }

    #[test]
    fn residential_denial_truth_table_preserves_the_reserved_credential() {
        let (user, residential) = provisioned();
        let mut cases = Vec::new();
        let mut r = residential.clone();
        r.groups.get_mut(DEFAULT_GROUP).unwrap().enabled = false;
        cases.push((user.clone(), r, EgressDeny::PoolDisabled));
        let mut r = residential.clone();
        r.groups.get_mut(DEFAULT_GROUP).unwrap().upstreams.clear();
        cases.push((user.clone(), r, EgressDeny::EmptyPool));
        let mut u = user.clone();
        u.entitlements.residential.as_mut().unwrap().slot_id = None;
        cases.push((u, residential.clone(), EgressDeny::MissingSlotBinding));
        let mut r = residential.clone();
        r.slots.clear();
        cases.push((user.clone(), r, EgressDeny::MissingSlot));
        let mut r = residential.clone();
        r.slots.push(r.slots[0]);
        cases.push((user.clone(), r, EgressDeny::AmbiguousSlot));
        let mut r = residential.clone();
        r.slots.push(Slot {
            index: 6,
            upstream_id: Uuid::from_u128(3),
        });
        cases.push((user.clone(), r, EgressDeny::AmbiguousSlot));
        let mut r = residential.clone();
        r.slots[0].index = crate::slots::MAX_SLOTS;
        cases.push((user.clone(), r, EgressDeny::InvalidSlotIndex));
        let mut r = residential.clone();
        r.groups.get_mut(DEFAULT_GROUP).unwrap().upstreams[0].id = Uuid::from_u128(3);
        cases.push((user.clone(), r, EgressDeny::MissingUpstream));
        let mut r = residential.clone();
        let duplicate = r.groups[DEFAULT_GROUP].upstreams[0].clone();
        r.groups
            .get_mut(DEFAULT_GROUP)
            .unwrap()
            .upstreams
            .push(duplicate);
        cases.push((user.clone(), r, EgressDeny::AmbiguousUpstream));
        let mut r = residential.clone();
        r.groups.get_mut(DEFAULT_GROUP).unwrap().upstreams[0].port = 0;
        cases.push((user.clone(), r, EgressDeny::InvalidUpstream));
        let mut r = residential.clone();
        r.groups
            .insert("foreign".into(), r.groups[DEFAULT_GROUP].clone());
        let mut u = user.clone();
        u.entitlements.residential.as_mut().unwrap().group_id = "foreign".into();
        cases.push((u, r, EgressDeny::UnsupportedGroup));
        for (u, r, reason) in cases {
            for protocol in [Protocol::Reality, Protocol::Hysteria2] {
                assert_eq!(
                    access_for(
                        &u,
                        &r,
                        protocol,
                        RequestedEgress::RequiredResidential,
                        false
                    ),
                    Err(reason)
                );
                assert_eq!(
                    access_for(&u, &r, protocol, RequestedEgress::Direct, false),
                    Ok(AuthorizedEgress::Direct)
                );
            }
            assert!(
                crate::hy2pool::is_resi_hy2(&u, &r),
                "allocation ownership survives temporary unavailability"
            );
            assert_eq!(u.credentials.hy2_resi_cred.as_deref(), Some("r000"));
            assert_eq!(r.hy2_pool.creds[0].secret, "reserved-secret");
            assert!(!reason.to_string().contains("reserved-secret"));
        }
    }

    #[test]
    fn hy2_requires_a_unique_owned_credential_but_reality_does_not() {
        let (user, residential) = provisioned();
        let mut cases = Vec::new();
        let mut u = user.clone();
        u.credentials.hy2_resi_cred = None;
        cases.push((u, residential.clone(), EgressDeny::MissingHy2Credential));
        let mut r = residential.clone();
        r.hy2_pool.creds.clear();
        cases.push((user.clone(), r, EgressDeny::MissingHy2Credential));
        let mut r = residential.clone();
        r.hy2_pool.creds.push(r.hy2_pool.creds[0].clone());
        cases.push((user.clone(), r, EgressDeny::InvalidHy2Credential));
        let mut r = residential.clone();
        let mut alias = r.hy2_pool.creds[0].clone();
        alias.id = "r001".into();
        r.hy2_pool.creds.push(alias);
        cases.push((user.clone(), r, EgressDeny::InvalidHy2Credential));
        let mut r = residential.clone();
        r.hy2_pool.creds[0].secret.clear();
        cases.push((user.clone(), r, EgressDeny::InvalidHy2Credential));
        for (u, r, reason) in cases {
            assert_eq!(
                access_for(
                    &u,
                    &r,
                    Protocol::Hysteria2,
                    RequestedEgress::RequiredResidential,
                    false
                ),
                Err(reason)
            );
            assert_eq!(
                access_for(
                    &u,
                    &r,
                    Protocol::Reality,
                    RequestedEgress::RequiredResidential,
                    false
                ),
                Ok(AuthorizedEgress::Residential(ResidentialBinding {
                    upstream_id: Uuid::from_u128(2),
                    slot_index: 6
                }))
            );
        }
    }

    #[test]
    fn a_reclaimed_credential_retains_history_without_losing_authorization() {
        let (user, mut residential) = provisioned();
        // assign_at reclaims eligible credentials without erasing the previous
        // release timestamp. Current ownership is the user's exact credential id.
        residential.hy2_pool.creds[0].released_at = Some("2026-09-28T00:00:00Z".into());
        assert_eq!(
            access_for(
                &user,
                &residential,
                Protocol::Hysteria2,
                RequestedEgress::RequiredResidential,
                false
            ),
            Ok(AuthorizedEgress::Residential(ResidentialBinding {
                upstream_id: Uuid::from_u128(2),
                slot_index: 6
            }))
        );
    }
}
