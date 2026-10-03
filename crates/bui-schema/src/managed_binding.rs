//! Private canonical projections for managed publication. No network/probe owner lives here.
//! Secret-bearing canonical bytes never leave this module or implement Debug/Serialize.
use crate::egress::{access_for, AuthorizedEgress, EgressDeny, RequestedEgress};
use crate::managed::{select_nodes, EgressIdentity, EvidenceStatus};
use crate::model::{Protocol, State, User};
use crate::nodes::{nodes_for, Transport};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error("managed path unavailable")]
    Unavailable,
    #[error("ambiguous managed account identity")]
    AmbiguousUser,
    #[error("managed revision exhausted")]
    RevisionOverflow,
    #[error("managed canonicalization failed")]
    Canonicalization,
}

/// Validate intent against the same authority as nodes_for, ignoring account/service outages.
/// This never creates a grant, repairs a binding, or substitutes a different identity.
pub fn selection_granted(user: &User, state: &State, identity: EgressIdentity) -> bool {
    let mut candidate = user.clone();
    candidate.disabled = false;
    let requested = match identity {
        EgressIdentity::Vps => RequestedEgress::Direct,
        EgressIdentity::Residential => RequestedEgress::RequiredResidential,
    };
    [Protocol::Hysteria2, Protocol::Reality]
        .into_iter()
        .any(|protocol| {
            !matches!(
                access_for(&candidate, &state.residential, protocol, requested, false),
                Err(EgressDeny::ProtocolNotGranted
                    | EgressDeny::DirectNotGranted
                    | EgressDeny::ResidentialNotGranted)
            )
        })
}

fn key(state: &State) -> Result<Vec<u8>, BindingError> {
    // Install/import generate 32 random bytes as hex. Do not accept a password/short key.
    if state.admin.jwt_secret.len() != 64 {
        return Err(BindingError::Unavailable);
    }
    hex::decode(&state.admin.jwt_secret).map_err(|_| BindingError::Unavailable)
}
fn digest(state: &State, domain: &[u8], value: &Value) -> Result<String, BindingError> {
    let bytes = serde_json::to_vec(value).map_err(|_| BindingError::Canonicalization)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&key(state)?).map_err(|_| BindingError::Unavailable)?;
    mac.update(domain);
    mac.update(&bytes);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Exact authorized service-path key. Subscriber secrets are deliberately excluded.
/// Returns no fingerprint at all when the key, selection, or selected path is unavailable.
pub fn path_fingerprint(state: &State, user: &User) -> Result<String, BindingError> {
    key(state)?;
    let identity = user.managed_egress.ok_or(BindingError::Unavailable)?;
    let nodes = select_nodes(&nodes_for(user, &state.node, &state.residential), identity);
    if nodes.is_empty() {
        return Err(BindingError::Unavailable);
    }
    let mut scope = Vec::new();
    let mut binding = None;
    for node in nodes {
        let (protocol, service) = match node.transport {
            Transport::Hysteria2 {
                sni, obfs_password, ..
            } => (
                Protocol::Hysteria2,
                json!({"protocol":"hysteria2", "sni":sni,"obfs":obfs_password}),
            ),
            Transport::Reality {
                public_key,
                short_id,
                server_name,
                fingerprint,
                flow,
                ..
            } => (
                Protocol::Reality,
                json!({"protocol":"reality","public_key":public_key,"short_id":short_id,"server_name":server_name,"fingerprint":fingerprint,"flow":flow,
                    "private_key":state.node.reality.private_key}),
            ),
        };
        if identity == EgressIdentity::Residential {
            let authorized = access_for(
                user,
                &state.residential,
                protocol,
                RequestedEgress::RequiredResidential,
                false,
            )
            .map_err(|_| BindingError::Unavailable)?;
            let AuthorizedEgress::Residential(b) = authorized else {
                return Err(BindingError::Unavailable);
            };
            if binding.is_some_and(|previous| previous != b) {
                return Err(BindingError::Unavailable);
            }
            binding = Some(b);
        }
        scope.push(json!({"host":node.host,"port":node.port,"hop":node.hop,"service":service}));
    }
    let supplier = if let Some(binding) = binding {
        let entitlement = user
            .entitlements
            .residential
            .as_ref()
            .ok_or(BindingError::Unavailable)?;
        let group = state
            .residential
            .groups
            .get(&entitlement.group_id)
            .ok_or(BindingError::Unavailable)?;
        let upstream = group
            .upstreams
            .iter()
            .find(|u| u.id == binding.upstream_id)
            .ok_or(BindingError::Unavailable)?;
        let mut ports = upstream.ports_allowed.clone();
        if let Some(ports) = ports.as_mut() {
            ports.sort_unstable();
            ports.dedup();
        }
        json!({"group":entitlement.group_id,"upstream":upstream.id,"slot":binding.slot_index,
            "kind":upstream.kind,"host":upstream.host,"port":upstream.port,"username":upstream.username,
            "password":upstream.password,"provider":upstream.provider,"ports_allowed":ports})
    } else {
        Value::Null
    };
    digest(
        state,
        b"bui-managed-path-v1\0",
        &json!({"node_id":state.node.id,
        "identity":identity,"dial_ip":state.node.public_ip,"scope":scope,"supplier":supplier}),
    )
}

fn evidence_semantics(status: &EvidenceStatus) -> Value {
    match status {
        EvidenceStatus::Unknown => json!({"status":"unknown"}),
        EvidenceStatus::Unsupported => json!({"status":"unsupported"}),
        EvidenceStatus::Verified(e) => {
            json!({"status":"verified","path":e.path_fingerprint,"identity":e.observed_identity})
        }
    }
}

/// Desired semantics remain observable while access_for denies rendering. This projection
/// is only input to the private semantic HMAC: it never authorizes nodes or creates a proof
/// key. Exact references (including missing/ambiguous matches) are retained; no pool-wide
/// selected upstream, health ranking, probe metadata, or alternate provider is substituted.
fn desired_semantics(state: &State, user: &User, identity: EgressIdentity) -> Value {
    let protocols: Vec<_> = [Protocol::Hysteria2, Protocol::Reality]
        .into_iter()
        .filter(|p| user.entitlements.protocols.contains(p))
        .collect();
    let residential = identity == EgressIdentity::Residential;
    let grant = if residential {
        if let Some(entitlement) = &user.entitlements.residential {
            let group = state.residential.groups.get(&entitlement.group_id);
            let selected_indices: BTreeSet<_> = state
                .residential
                .slots
                .iter()
                .filter(|slot| Some(slot.upstream_id) == entitlement.slot_id)
                .map(|slot| slot.index)
                .collect();
            let mut slots: Vec<_> = state
                .residential
                .slots
                .iter()
                .filter(|slot| {
                    Some(slot.upstream_id) == entitlement.slot_id
                        || selected_indices.contains(&slot.index)
                })
                .map(|slot| (slot.index, slot.upstream_id))
                .collect();
            slots.sort_unstable();
            let mut providers: Vec<_> = group.into_iter().flat_map(|g| &g.upstreams)
                .filter(|upstream| Some(upstream.id) == entitlement.slot_id)
                .map(|upstream| {
                    let mut ports = upstream.ports_allowed.clone();
                    if let Some(ports) = &mut ports { ports.sort_unstable(); ports.dedup(); }
                    json!({"id":upstream.id,"kind":upstream.kind,"host":upstream.host,"port":upstream.port,
                        "username":upstream.username,"password":upstream.password,"provider":upstream.provider,"ports_allowed":ports})
                }).collect();
            providers.sort_by_cached_key(Value::to_string);
            json!({"group":entitlement.group_id,"upstream_id":entitlement.slot_id,
                "group_enabled":group.map(|g|g.enabled),"slots":slots,"providers":providers})
        } else {
            Value::Null
        }
    } else {
        json!({"direct":user.entitlements.direct})
    };
    let mut services = Vec::new();
    if protocols.contains(&Protocol::Hysteria2) {
        let subscriber = if residential {
            let reference = user.credentials.hy2_resi_cred.as_ref();
            let names: BTreeSet<_> = state
                .residential
                .hy2_pool
                .creds
                .iter()
                .filter(|cred| Some(&cred.id) == reference)
                .map(|cred| &cred.name)
                .collect();
            let mut credentials: Vec<_> = state
                .residential
                .hy2_pool
                .creds
                .iter()
                .filter(|cred| Some(&cred.id) == reference || names.contains(&cred.name))
                .map(|cred| json!({"id":cred.id,"name":cred.name,"secret":cred.secret}))
                .collect();
            credentials.sort_by_cached_key(Value::to_string);
            json!({"reference":reference,"credentials":credentials,"desired_password":user.credentials.hy2_password})
        } else {
            json!({"username":user.username,"password":user.credentials.hy2_password})
        };
        let (port, hop) = if residential {
            (
                state.node.ports.hy2_resi,
                Some(state.node.ports.hy2_resi_hop),
            )
        } else {
            (state.node.ports.hy2, state.node.ports.hy2_hop)
        };
        services.push(
            json!({"protocol":"hysteria2","port":port,"hop":hop,"sni":state.node.domain,
            "obfs":state.node.obfs,"subscriber":subscriber}),
        );
    }
    if protocols.contains(&Protocol::Reality) {
        let port = if residential {
            state.node.ports.reality_resi
        } else {
            state.node.ports.reality_direct
        };
        services.push(json!({"protocol":"reality","port":port,"tls":state.node.reality,"uuid":user.credentials.vless_uuid}));
    }
    json!({"identity":identity,"grant":grant,"protocols":protocols,"services":services,
        "node_id":state.node.id,"domain":state.node.domain,"dial_ip":state.node.public_ip,"disabled":user.disabled})
}

/// Opaque equality stamp; no Debug/Serialize and no secret-bearing data exposed.
#[derive(Clone, PartialEq, Eq)]
pub struct SemanticStamp(String);

impl SemanticStamp {
    /// ETag over actual response bytes, scoped to one account/target/revision.
    /// The existing private semantic HMAC is a pseudorandom derived key; a distinct
    /// domain and length-delimited inputs prevent reuse as a semantic/path MAC.
    /// Neither its bytes nor the installation JWT key are exposed to the caller.
    pub fn profile_etag(
        &self,
        account: uuid::Uuid,
        target: &str,
        revision: u64,
        body: &[u8],
    ) -> Result<String, BindingError> {
        if self.0.len() != 64 {
            return Err(BindingError::Unavailable);
        }
        let derived_key = hex::decode(&self.0).map_err(|_| BindingError::Unavailable)?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&derived_key).map_err(|_| BindingError::Unavailable)?;
        mac.update(b"bui-managed-profile-etag-v1\0");
        mac.update(account.as_bytes());
        mac.update(&revision.to_be_bytes());
        mac.update(&(target.len() as u64).to_be_bytes());
        mac.update(target.as_bytes());
        mac.update(&(body.len() as u64).to_be_bytes());
        mac.update(body);
        Ok(format!("\"{}\"", hex::encode(mac.finalize().into_bytes())))
    }
}

pub fn semantic_stamp(state: &State, user: &User) -> Result<SemanticStamp, BindingError> {
    let Some(identity) = user.managed_egress else {
        return Ok(SemanticStamp("unset".into()));
    };
    if key(state).is_err() {
        // Deterministic, nonsecret unavailable category permits legacy/key repair.
        return Ok(SemanticStamp(format!("invalid-key:{identity:?}")));
    }
    let path = match path_fingerprint(state, user) {
        Ok(path) => Some(path),
        Err(BindingError::Unavailable) => None,
        Err(error) => return Err(error),
    };
    let caps = path
        .as_ref()
        .and_then(|p| state.managed_egress_capabilities.get(p))
        .cloned()
        .unwrap_or_default();
    let nodes = select_nodes(&nodes_for(user, &state.node, &state.residential), identity);
    Ok(SemanticStamp(digest(
        state,
        b"bui-managed-semantics-v1\0",
        &json!({
            "identity":identity,"path":path,"nodes":nodes,"username":user.username,
            "desired":desired_semantics(state,user,identity),
            "token":user.sub_token,"legacy_disabled":user.legacy_sub_disabled,
            "dial_ip":state.node.public_ip,
            "capabilities":[evidence_semantics(&caps.v4_tcp),evidence_semantics(&caps.v4_udp),evidence_semantics(&caps.v6_tcp),evidence_semantics(&caps.v6_udp)]
        }),
    )?))
}

/// Reconcile all writers centrally, by stable account ID. Calculate every result first:
/// overflow/ambiguous IDs/canonicalization never leave a partially revised candidate.
pub fn reconcile_revisions(old: &State, next: &mut State) -> Result<(), BindingError> {
    for state in [old, &*next] {
        let mut ids = BTreeSet::new();
        if state.users.iter().any(|u| !ids.insert(u.user_id)) {
            return Err(BindingError::AmbiguousUser);
        }
    }
    let revisions: Result<Vec<_>, BindingError> = next
        .users
        .iter()
        .map(|user| {
            if user.managed_egress.is_none() {
                return Ok(None);
            }
            let previous = old.users.iter().find(|u| u.user_id == user.user_id);
            let Some(previous) = previous.filter(|u| u.managed_egress.is_some()) else {
                return Ok(Some(1));
            };
            let Some(revision) = previous.managed_profile_revision.filter(|r| *r > 0) else {
                return Ok(Some(1));
            };
            if semantic_stamp(old, previous)? == semantic_stamp(next, user)? {
                return Ok(Some(revision));
            }
            revision
                .checked_add(1)
                .map(Some)
                .ok_or(BindingError::RevisionOverflow)
        })
        .collect();
    for (user, revision) in next.users.iter_mut().zip(revisions?) {
        user.managed_profile_revision = revision;
    }
    Ok(())
}

#[cfg(test)]
mod profile_etag_tests {
    use super::*;

    #[test]
    fn profile_etag_scopes_body_account_revision_target_and_derived_key() {
        let stamp = SemanticStamp("01".repeat(32));
        let id = uuid::Uuid::from_u128(1);
        let base = stamp
            .profile_etag(id, "v2rayn-sb1142-macos", 1, b"{}")
            .unwrap();
        assert_eq!(
            base,
            stamp
                .profile_etag(id, "v2rayn-sb1142-macos", 1, b"{}")
                .unwrap()
        );
        for changed in [
            stamp.profile_etag(uuid::Uuid::from_u128(2), "v2rayn-sb1142-macos", 1, b"{}"),
            stamp.profile_etag(id, "other", 1, b"{}"),
            stamp.profile_etag(id, "v2rayn-sb1142-macos", 2, b"{}"),
            stamp.profile_etag(id, "v2rayn-sb1142-macos", 1, b"{ }"),
            SemanticStamp("02".repeat(32)).profile_etag(id, "v2rayn-sb1142-macos", 1, b"{}"),
        ] {
            assert_ne!(base, changed.unwrap());
        }
        for invalid in ["unset", "invalid-key:Vps", "zz", "aa"] {
            assert!(SemanticStamp(invalid.into())
                .profile_etag(id, "v2rayn-sb1142-macos", 1, b"{}")
                .is_err());
        }
    }
}
