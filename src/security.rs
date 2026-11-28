//! Security Contexts and principals, the design's "Security Contexts and Principals", as far as its third part: a
//! domain, the root SC, whose ID is made from its founder's first key; principals, users and systems, each a DataSet of
//! their domain, with their keys, each committing to the next, and their DirectoryInfo, as records of their own; SCs,
//! joined by membership into a graph, each principal's role the best of its paths; membership records kept as a
//! history, never replaced; delegations, by which an actor acts for a subject while the subject is away; and requests
//! signed by their subject and their actor, checked by the subject's role. Rolling and replacing keys, thresholds,
//! signed, notarised and encrypted records, wrapped keys and sharing across domains are later, and add to this without
//! changing it.
//!
//! Every record here is signed by a principal over its canonical JSON, see id.rs, without its signature, so that every
//! node signs and checks the same bytes, and its version is when it was signed, its `at`, so that a record signed
//! earlier, put again later, never replaces one signed later. The records are kept in the store as any others are, so
//! that they replicate, in DataSets found by their hash keys alone, so that working out who may do what,
//! [Security::load], is a few queries, which DynamoDB answers as it does any other, never a listing of DataSets: the
//! domain's list of its principals, `<domain>/principals`; each principal's record and keys, `<domain>/principal/<ID>`;
//! the SCs' records, `<domain>/security.sc`; each SC's members, `<domain>/security.member/<SC ID>`; each subject's
//! delegations, `<domain>/security.delegation/<subject ID>`; and what is said of each principal in each SC,
//! `<domain>/directory/<principal ID>/<SC ID>`.
//!
//! Who may do what is worked out in the order the records were signed, each judged only by what the records before it
//! made valid, from the domain's founder, so that two memberships, e.g. around a loop of SCs, never make each other
//! valid, and of two Admins removing each other, the earlier removal wins.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Id, Item, Micros, Part, Store, Version};

/// The algorithm of a principal's signing key, see PublicKey.
pub const SIGN_ALG: &str = "Ed25519";
/// The algorithm of a principal's, or an SC's, key exchange key, see PublicKey.
pub const KEX_ALG: &str = "X25519";
/// The hash a domain's IDs are made with, which its record names, see the design's IDs.
pub const HASH: &str = "sha256";
/// The kind of each principal's DataSet, its record and keys, see PrincipalRecord::hash_key.
pub const PRINCIPAL_KIND: &str = "principal";
/// The kind of the DataSet which lists a domain's principals, by their IDs, see PrincipalRecord::items.
pub const PRINCIPALS_KIND: &str = "principals";
/// The kind of the DataSets of what is said of each principal in each SC, see DirectoryRecord::hash_key.
pub const DIRECTORY_KIND: &str = "directory";
/// The kind of the DataSet of a domain's SCs' records, by their IDs, see ScRecord::hash_key.
pub const SC_KIND: &str = "security.sc";
/// The kind of the DataSet of each SC's members, see MemberRecord::hash_key.
pub const MEMBER_KIND: &str = "security.member";
/// The kind of the DataSet of each subject's delegations, by the actors' IDs, see DelegationRecord::hash_key.
pub const DELEGATION_KIND: &str = "security.delegation";
/// What a principal is, see PrincipalRecord::kind: a person, or a system, a program which acts by itself, e.g. a
/// capture device or an installation of an app, which is its role, not its kind.
pub const USER: &str = "user";
pub const SYSTEM: &str = "system";
/// The sort key of a principal's record in its DataSet, and the start of its keys', see PrincipalRecord::hash_key.
pub const RECORD_SK: &str = "-";
pub const KEY_SK: &str = "key/";
/// The attribute of a DirectoryRecord which names its subject, and the label of the statement which gives it.
pub const DISPLAY_NAME: &str = "displayName";
pub const NAME_LABEL: &str = "name";
/// What a signed request's Authorization header starts with, see sign_request.
pub const SCHEME: &str = "Bruce-Signed ";
/// How far, in seconds, a signed request's time may be from the node's, see Security::check_request.
pub const REQUEST_WINDOW: i64 = 5 * 60;

/// Whether the DataSet hk holds security or identity records, which a node gives only to those entitled to them, see
/// the design's "Servers and clients".
pub fn is_security_data_set(hk: &str) -> bool {
    let kind = hk.split('/').nth(1).unwrap_or_default();
    [
        PRINCIPAL_KIND,
        PRINCIPALS_KIND,
        DIRECTORY_KIND,
        SC_KIND,
        MEMBER_KIND,
        DELEGATION_KIND,
    ]
    .contains(&kind)
}

/// A member's role in an SC, from the least to the most it may do, so that the lower of two is their minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Reads the SC's records.
    Read,
    /// Puts them.
    Write,
    /// Adds and removes members, and does what Write does.
    Admin,
}

/// A public key, with its algorithm, so that one made later may use another, its bytes in URL-safe base64.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey {
    pub alg: String,
    pub key: String,
}

/// A principal's record, its identity, which every node of its domain has, see the design's Principals: what its ID is
/// made from, and who admitted it to the domain. Its keys and what is said of it are records of their own, see
/// KeyRecord and DirectoryRecord.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRecord {
    /// Its ID, see PrincipalRecord::id_of.
    pub id: String,
    /// The domain it belongs to.
    pub domain: String,
    /// What it is: USER, a person, or SYSTEM, a program which acts by itself.
    pub kind: String,
    /// When it was made, see Micros::to_text.
    pub made: String,
    /// 48 random bits, as hex, so that two principals made at once differ.
    pub nonce: String,
    /// Its first signing key, from which its ID is made.
    pub first: PublicKey,
    /// The hash of its next keys, see next_of, which it keeps unused, so that a key taken cannot roll its keys.
    pub next: String,
    /// When it was admitted to the domain, its version.
    pub at: String,
    /// Who signed it: itself, if it is the domain's founder, otherwise an Admin of the domain who admitted it.
    pub by: String,
    pub sig: String,
}

/// One of a principal's keys, see the design's Keys: its signing key and its key exchange key, made together, when it
/// became valid, the hash of the keys after it, and, once rolled, lost or stolen, when it stopped being valid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRecord {
    pub principal: String,
    /// Its key ID, "1" for the first.
    pub id: String,
    pub sign: PublicKey,
    pub kex: PublicKey,
    /// When it became valid, see Micros::to_text.
    pub from: String,
    /// The hash of the next keys, see next_of.
    pub next: String,
    /// When it stopped being valid, for any reason: what it signed before stays good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// When this record was signed, its version.
    pub at: String,
    pub by: String,
    pub sig: String,
}

/// What an issuer says of a principal, its subject, readable by the SC it is in, see the design's DirectoryInfo, e.g.
/// its `displayName`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryRecord {
    pub subject: String,
    pub sc: String,
    pub issuer: String,
    /// The issuer's label for it, e.g. NAME_LABEL, so that an issuer may say several things of one subject.
    pub label: String,
    pub attributes: serde_json::Map<String, Value>,
    pub at: String,
    pub by: String,
    pub sig: String,
}

/// What a domain's ID is made from beside when it was made, see ScRecord::domain_id_of: its founder's first signing key
/// and the hash of their next, and a nonce, so that anyone can check that the domain's record is its founder's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Founder {
    pub first: PublicKey,
    pub next: String,
    pub nonce: String,
}

/// An SC's record, see the design's Security Contexts; a domain's is its root's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScRecord {
    /// Its ID, made from its creator, when it was made and its first key, see ScRecord::id_of, or, for a domain, see
    /// ScRecord::domain_id_of.
    pub id: String,
    pub name: String,
    /// The principal which made it, its first Admin.
    pub creator: String,
    /// When it was made, see Micros::to_text.
    pub made: String,
    /// Its key exchange keys' public halves, by key ID; the first, "1", is always kept.
    pub keys: BTreeMap<String, PublicKey>,
    /// Whether it is a domain, the root SC, from which its other SCs and members are checked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub root: bool,
    /// For a domain, what its ID is made from, see Founder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub founder: Option<Founder>,
    /// For a domain, the hash its IDs are made with, see HASH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// For a domain, the SC a record is in when it does not say, see the design's Security Contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Whether its records are to be encrypted, see the design's Encryption; not yet used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub encrypt: bool,
    /// Whether its records must be signed, see the design's Signed records; not yet used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signed: bool,
    /// When this record was signed, its version.
    pub at: String,
    pub by: String,
    pub sig: String,
}

/// A membership of an SC, or a change to one, see the design's Membership: each is a record of its own, so that the
/// history is kept and worked through in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    /// The SC it is a membership of.
    pub sc: String,
    /// The member, a principal's ID, or a parent SC's, see the design's "The SC graph".
    pub member: String,
    pub role: Role,
    /// When it was first made a member, see Micros::to_text.
    pub since: String,
    /// When this record was made, from when it counts.
    pub at: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    /// Whether it is the domain's backup Admin, see the design's "Replacing a key"; not yet used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backup: bool,
    pub by: String,
    pub sig: String,
}

/// A delegation, see the design's "Subjects and actors": the subject lets the actor act for it in the domain while the
/// subject is away, up to role in each SC, which is never more than Write, so that adding a member always needs the
/// subject itself. It is signed by the subject, and ends when until is set, or when the subject's own membership does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub subject: String,
    pub actor: String,
    pub role: Role,
    /// When it was first made, see Micros::to_text.
    pub since: String,
    /// When this record was made, or the delegation ended.
    pub at: String,
    /// When it ended, if it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub by: String,
    pub sig: String,
}

/// A record signed by a principal, see sign and Security's checks.
pub trait Signed: Serialize {
    /// Who signed it, `<principal ID>/<key ID>`.
    fn by(&self) -> &str;
    fn sig(&self) -> &str;
    /// When it was signed, its version.
    fn at(&self) -> &str;
    fn set_signature(&mut self, by: String, sig: String);
}

macro_rules! signed {
    ($type:ty) => {
        impl Signed for $type {
            fn by(&self) -> &str {
                &self.by
            }
            fn sig(&self) -> &str {
                &self.sig
            }
            fn at(&self) -> &str {
                &self.at
            }
            fn set_signature(&mut self, by: String, sig: String) {
                self.by = by;
                self.sig = sig;
            }
        }
    };
}
signed!(PrincipalRecord);
signed!(KeyRecord);
signed!(DirectoryRecord);
signed!(ScRecord);
signed!(MemberRecord);
signed!(DelegationRecord);

/// What a record's signature is made over: its canonical JSON without its signature.
fn signed_bytes(record: &impl Signed) -> anyhow::Result<Vec<u8>> {
    let mut value = serde_json::to_value(record)?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("sig");
    }
    Ok(crate::id::canonical(&value)?.into_bytes())
}

/// Signs record as the principal whose secret is by.
fn sign(record: &mut impl Signed, by: &PrincipalSecret) -> anyhow::Result<()> {
    record.set_signature(by.signer(), String::new());
    let signature = by.sign_bytes(&signed_bytes(record)?);
    let signer = by.signer();
    record.set_signature(signer, signature);
    Ok(())
}

/// The principal and key ID a signer names, `<principal ID>/<key ID>`.
fn signer_of(by: &str) -> anyhow::Result<(&str, &str)> {
    by.rsplit_once('/')
        .ok_or_else(|| anyhow!("{by} is not a principal and key ID"))
}

/// Checks that record is signed by key.
fn verify_with(record: &impl Signed, key: &VerifyingKey) -> anyhow::Result<()> {
    let signature = signature_of(record.sig())?;
    key.verify_strict(&signed_bytes(record)?, &signature)
        .map_err(|_| anyhow!("The signature by {} is not right", record.by()))
}

/// The verifying key of a public signing key.
fn verifying_key_of(key: &PublicKey) -> anyhow::Result<VerifyingKey> {
    if key.alg != SIGN_ALG {
        bail!("A key of {} is not known", key.alg);
    }
    VerifyingKey::from_bytes(&unbase64::<32>(&key.key)?).map_err(|error| anyhow!("It is not a key, {error}"))
}

fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unbase64<const N: usize>(text: &str) -> anyhow::Result<[u8; N]> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| anyhow!("{text} is not {N} bytes of URL-safe base64"))
}

fn signature_of(text: &str) -> anyhow::Result<ed25519_dalek::Signature> {
    Ok(ed25519_dalek::Signature::from_bytes(&unbase64::<64>(text)?))
}

fn random<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).map_err(|error| anyhow!("Unable to make random bytes, {error}"))?;
    Ok(bytes)
}

fn nonce() -> anyhow::Result<String> {
    Ok(random::<6>()?.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn micros(text: &str) -> anyhow::Result<Micros> {
    Micros::parse(text)
}

/// It as an item of the DataSet hk, sort key sk, its version when it was signed, by origin, the node putting it.
fn item_of(hk: &str, sk: &str, record: &impl Signed, origin: &str) -> anyhow::Result<Item> {
    Ok(Item::new(
        hk,
        sk,
        Version::new(micros(record.at())?, origin),
        serde_json::to_value(record)?,
    ))
}

/// A pair of private keys, made together: a signing key and a key exchange key.
#[derive(Clone)]
struct KeyPair {
    sign: [u8; 32],
    kex: [u8; 32],
}

impl KeyPair {
    fn new() -> anyhow::Result<KeyPair> {
        Ok(KeyPair {
            sign: random()?,
            kex: random()?,
        })
    }

    fn sign_public(&self) -> PublicKey {
        PublicKey {
            alg: SIGN_ALG.to_string(),
            key: base64(SigningKey::from_bytes(&self.sign).verifying_key().as_bytes()),
        }
    }

    fn kex_public(&self) -> PublicKey {
        let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(self.kex));
        PublicKey {
            alg: KEX_ALG.to_string(),
            key: base64(public.as_bytes()),
        }
    }

    /// The hash of its public keys, which the keys before it commit to, see next_of.
    fn hash(&self) -> anyhow::Result<String> {
        next_of(&self.sign_public(), &self.kex_public())
    }
}

/// The hash of a key pair's public keys, which the key before it names as its `next`, so that only whoever holds them
/// can roll a principal's keys, as KERI's pre-rotation does, see the design's Keys.
pub fn next_of(sign: &PublicKey, kex: &PublicKey) -> anyhow::Result<String> {
    Ok(Id::of(&serde_json::json!({ "next": { "sign": sign, "kex": kex } }))?.to_string())
}

/// A principal's private keys, which never leave it: a system keeps them in its own storage, encrypted, a user in their
/// key file, as the text to_text gives: its current keys, and its next, unused. Debug output hides them.
#[derive(Clone)]
pub struct PrincipalSecret {
    id: Id,
    domain: Id,
    key_id: String,
    current: KeyPair,
    next: KeyPair,
}

impl std::fmt::Debug for PrincipalSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalSecret")
            .field("id", &self.id)
            .field("domain", &self.domain)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// How a PrincipalSecret is kept, see to_text.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretText {
    id: String,
    domain: String,
    key: String,
    sign: String,
    kex: String,
    next_sign: String,
    next_kex: String,
}

/// A new principal, see NewPrincipal::new: its secret, its record, signed by itself until an Admin of its domain
/// admits it, see PrincipalRecord::admitted_by, and its first key's record, signed by itself.
pub struct NewPrincipal {
    pub secret: PrincipalSecret,
    pub record: PrincipalRecord,
    pub key: KeyRecord,
}

impl NewPrincipal {
    /// A new principal of domain, of kind, USER or SYSTEM, with its first keys and its next.
    pub fn new(domain: &Id, kind: &str) -> anyhow::Result<NewPrincipal> {
        Self::with_keys(domain, kind, KeyPair::new()?, KeyPair::new()?)
    }

    fn with_keys(domain: &Id, kind: &str, current: KeyPair, next: KeyPair) -> anyhow::Result<NewPrincipal> {
        if kind != USER && kind != SYSTEM {
            bail!("A principal is a {USER} or a {SYSTEM}, not {kind}");
        }
        let made = Micros::now().to_text();
        let nonce = nonce()?;
        let first = current.sign_public();
        let next_hash = next.hash()?;
        let id = PrincipalRecord::id_of(domain, kind, &made, &nonce, &first, &next_hash)?;
        let secret = PrincipalSecret {
            id: id.clone(),
            domain: domain.clone(),
            key_id: "1".to_string(),
            current: current.clone(),
            next,
        };
        let mut record = PrincipalRecord {
            id: id.to_string(),
            domain: domain.to_string(),
            kind: kind.to_string(),
            made: made.clone(),
            nonce,
            first: first.clone(),
            next: next_hash.clone(),
            at: made.clone(),
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut record, &secret)?;
        let mut key = KeyRecord {
            principal: id.to_string(),
            id: "1".to_string(),
            sign: first,
            kex: current.kex_public(),
            from: made.clone(),
            next: next_hash,
            until: None,
            at: made,
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut key, &secret)?;
        Ok(NewPrincipal { secret, record, key })
    }
}

impl PrincipalSecret {
    pub fn id(&self) -> &Id {
        &self.id
    }

    pub fn domain(&self) -> &Id {
        &self.domain
    }

    /// Who it signs as, `<principal ID>/<key ID>`, see the design's Principals.
    pub fn signer(&self) -> String {
        format!("{}/{}", self.id, self.key_id)
    }

    /// The signature of bytes, in URL-safe base64.
    pub fn sign_bytes(&self, bytes: &[u8]) -> String {
        base64(&SigningKey::from_bytes(&self.current.sign).sign(bytes).to_bytes())
    }

    /// The text to keep it as, secret, which from_text reads.
    pub fn to_text(&self) -> String {
        serde_json::to_string(&SecretText {
            id: self.id.to_string(),
            domain: self.domain.to_string(),
            key: self.key_id.clone(),
            sign: base64(&self.current.sign),
            kex: base64(&self.current.kex),
            next_sign: base64(&self.next.sign),
            next_kex: base64(&self.next.kex),
        })
        .unwrap_or_default()
    }

    pub fn from_text(text: &str) -> anyhow::Result<PrincipalSecret> {
        let kept: SecretText = serde_json::from_str(text).context("Not a principal's secret")?;
        Ok(PrincipalSecret {
            id: Id::parse(&kept.id)?,
            domain: Id::parse(&kept.domain)?,
            key_id: kept.key,
            current: KeyPair {
                sign: unbase64(&kept.sign)?,
                kex: unbase64(&kept.kex)?,
            },
            next: KeyPair {
                sign: unbase64(&kept.next_sign)?,
                kex: unbase64(&kept.next_kex)?,
            },
        })
    }
}

impl PrincipalRecord {
    /// The ID of a principal of domain, of kind, made at made with nonce, with first, its first signing key, and next,
    /// the hash of its next keys, see the design's Principals.
    pub fn id_of(
        domain: &Id,
        kind: &str,
        made: &str,
        nonce: &str,
        first: &PublicKey,
        next: &str,
    ) -> anyhow::Result<Id> {
        Id::of(&serde_json::json!({ "principal": {
            "domain": domain.as_str(),
            "kind": kind,
            "made": made,
            "nonce": nonce,
            "alg": first.alg,
            "key": first.key,
            "next": next,
        } }))
    }

    /// Checks that its ID is the one what it says it is made from gives.
    pub fn check_id(&self) -> anyhow::Result<()> {
        let id = Self::id_of(
            &Id::parse(&self.domain)?,
            &self.kind,
            &self.made,
            &self.nonce,
            &self.first,
            &self.next,
        )?;
        if id.as_str() != self.id {
            bail!("The principal {} is not the one its record says", self.id);
        }
        Ok(())
    }

    /// Checks that it is the record of its ID, and signed by its own first key, as a new principal's is, e.g. in a
    /// request to join a domain, before an Admin admits it.
    pub fn verify_own(&self) -> anyhow::Result<()> {
        self.check_id()?;
        if self.by != format!("{}/1", self.id) {
            bail!("The principal {}'s record is not signed by itself", self.id);
        }
        verify_with(self, &verifying_key_of(&self.first)?)
    }

    /// Whether it is a person's, rather than a system's, see USER.
    pub fn is_user(&self) -> bool {
        self.kind == USER
    }

    /// The record admitting it to its domain, signed by by, an Admin of the domain, see Security.
    pub fn admitted_by(&self, by: &PrincipalSecret) -> anyhow::Result<PrincipalRecord> {
        let mut record = PrincipalRecord {
            at: Micros::now().to_text(),
            ..self.clone()
        };
        sign(&mut record, by)?;
        Ok(record)
    }

    /// The hash key of the principal id's DataSet in store's domain, its record and its keys.
    pub fn hash_key(store: &Store, id: &Id) -> anyhow::Result<String> {
        store.hash_key(PRINCIPAL_KIND, &[Part::Id(id.clone())])
    }

    /// The hash key of the DataSet which lists store's domain's principals.
    pub fn list_hash_key(store: &Store) -> anyhow::Result<String> {
        store.hash_key(PRINCIPALS_KIND, &[])
    }

    /// It as the items to put in store, by origin: its record, and its line in the domain's list of principals.
    pub fn items(&self, store: &Store, origin: &str) -> anyhow::Result<Vec<Item>> {
        let id = Id::parse(&self.id)?;
        let listed = Item::new(
            &Self::list_hash_key(store)?,
            &self.id,
            Version::new(micros(&self.at)?, origin),
            serde_json::json!({ "kind": self.kind }),
        );
        Ok(vec![
            item_of(&Self::hash_key(store, &id)?, RECORD_SK, self, origin)?,
            listed,
        ])
    }
}

impl KeyRecord {
    /// It as an item to put in store, by origin, in its principal's DataSet.
    pub fn item(&self, store: &Store, origin: &str) -> anyhow::Result<Item> {
        item_of(
            &PrincipalRecord::hash_key(store, &Id::parse(&self.principal)?)?,
            &format!("{KEY_SK}{}", self.id),
            self,
            origin,
        )
    }

    /// Whether it was valid at at: before its until, if it has one. Its from is not checked, as the clocks of the node
    /// which made it and the node checking may differ a little, and a key cannot have signed anything before it was
    /// made.
    pub fn valid_at(&self, at: Micros) -> bool {
        self.until
            .as_deref()
            .is_none_or(|until| micros(until).is_ok_and(|until| at < until))
    }
}

impl DirectoryRecord {
    /// What issuer says of subject in sc, under label, e.g. its name, signed by issuer.
    pub fn new(
        subject: &Id,
        sc: &Id,
        label: &str,
        attributes: serde_json::Map<String, Value>,
        issuer: &PrincipalSecret,
    ) -> anyhow::Result<DirectoryRecord> {
        let mut record = DirectoryRecord {
            subject: subject.to_string(),
            sc: sc.to_string(),
            issuer: issuer.id().to_string(),
            label: label.to_string(),
            attributes,
            at: Micros::now().to_text(),
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut record, issuer)?;
        Ok(record)
    }

    /// The name issuer gives subject in sc, see DISPLAY_NAME.
    pub fn name(subject: &Id, sc: &Id, name: &str, issuer: &PrincipalSecret) -> anyhow::Result<DirectoryRecord> {
        let mut attributes = serde_json::Map::new();
        attributes.insert(DISPLAY_NAME.to_string(), Value::from(name));
        Self::new(subject, sc, NAME_LABEL, attributes, issuer)
    }

    /// The hash key of what is said of subject in sc, in store's domain.
    pub fn hash_key(store: &Store, subject: &Id, sc: &Id) -> anyhow::Result<String> {
        store.hash_key(DIRECTORY_KIND, &[Part::Id(subject.clone()), Part::Id(sc.clone())])
    }

    pub fn item(&self, store: &Store, origin: &str) -> anyhow::Result<Item> {
        item_of(
            &Self::hash_key(store, &Id::parse(&self.subject)?, &Id::parse(&self.sc)?)?,
            &format!("{}#{}", self.issuer, self.label),
            self,
            origin,
        )
    }
}

/// An SC's private keys, by key ID: its key exchange key and its shared symmetric key, kept by its creator, until they
/// are wrapped for its members, which is later. Debug output hides them.
#[derive(Clone)]
pub struct ScSecret {
    pub id: Id,
    keys: ScKeys,
}

impl std::fmt::Debug for ScSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScSecret").field("id", &self.id).finish_non_exhaustive()
    }
}

/// An SC's private keys by key ID, its key exchange key and its shared key, see ScSecret.
type ScKeys = BTreeMap<String, ([u8; 32], [u8; 32])>;

impl ScSecret {
    /// New keys for an SC, and the public half of its key exchange key.
    fn fresh() -> anyhow::Result<(ScKeys, PublicKey)> {
        let kex = random::<32>()?;
        let shared = random::<32>()?;
        let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(kex));
        let first = PublicKey {
            alg: KEX_ALG.to_string(),
            key: base64(public.as_bytes()),
        };
        Ok((BTreeMap::from([("1".to_string(), (kex, shared))]), first))
    }

    /// The text to keep it as, secret, which from_text reads: the SC's ID, and each key ID's key exchange and shared
    /// keys, in URL-safe base64.
    pub fn to_text(&self) -> String {
        let keys: BTreeMap<&String, [String; 2]> = self
            .keys
            .iter()
            .map(|(key_id, (kex, shared))| (key_id, [base64(kex), base64(shared)]))
            .collect();
        serde_json::json!({ "id": self.id.as_str(), "keys": keys }).to_string()
    }

    pub fn from_text(text: &str) -> anyhow::Result<ScSecret> {
        let value: Value = serde_json::from_str(text).context("Not an SC's secret")?;
        let id = Id::parse(value["id"].as_str().unwrap_or_default())?;
        let mut keys = BTreeMap::new();
        for (key_id, pair) in value["keys"].as_object().into_iter().flatten() {
            let (Some(kex), Some(shared)) = (pair[0].as_str(), pair[1].as_str()) else {
                bail!("The SC {id}'s key {key_id} is not as it should be");
            };
            keys.insert(key_id.clone(), (unbase64(kex)?, unbase64(shared)?));
        }
        Ok(ScSecret { id, keys })
    }
}

/// A new domain, see Founding::new: its founder, a user, their first key, the domain's record and its secret keys.
pub struct Founding {
    pub founder: NewPrincipal,
    pub domain: ScRecord,
    pub domain_secret: ScSecret,
}

impl Founding {
    /// A new domain, named name, founded by a new user principal, made with it, see the design's Domains: the user's
    /// keys are made first, then the domain's ID from them, then the user's principal in it, with the same first key,
    /// which signs the domain's record, as its first Admin.
    pub fn new(name: &str) -> anyhow::Result<Founding> {
        let current = KeyPair::new()?;
        let next = KeyPair::new()?;
        let made = Micros::now().to_text();
        let founder_keys = Founder {
            first: current.sign_public(),
            next: next.hash()?,
            nonce: nonce()?,
        };
        let id = ScRecord::domain_id_of(&made, &founder_keys)?;
        let founder = NewPrincipal::with_keys(&id, USER, current, next)?;
        let (keys, first) = ScSecret::fresh()?;
        let mut domain = ScRecord {
            id: id.to_string(),
            name: name.to_string(),
            creator: founder.secret.id().to_string(),
            made: made.clone(),
            keys: BTreeMap::from([("1".to_string(), first)]),
            root: true,
            founder: Some(founder_keys),
            hash: Some(HASH.to_string()),
            default: None,
            encrypt: false,
            signed: false,
            at: made,
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut domain, &founder.secret)?;
        Ok(Founding {
            founder,
            domain_secret: ScSecret { id, keys },
            domain,
        })
    }
}

impl ScRecord {
    /// A new SC under a domain, named name, made by creator, its first Admin, and its secret keys, and its record,
    /// signed, for the caller to put, see item.
    pub fn new(name: &str, creator: &PrincipalSecret) -> anyhow::Result<(ScSecret, ScRecord)> {
        let (keys, first) = ScSecret::fresh()?;
        let made = Micros::now().to_text();
        let id = Self::id_of(creator.id(), &made, &first)?;
        let mut record = ScRecord {
            id: id.to_string(),
            name: name.to_string(),
            creator: creator.id().to_string(),
            made: made.clone(),
            keys: BTreeMap::from([("1".to_string(), first)]),
            root: false,
            founder: None,
            hash: None,
            default: None,
            encrypt: false,
            signed: false,
            at: made,
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut record, creator)?;
        Ok((ScSecret { id, keys }, record))
    }

    /// The ID of an SC made by creator at made with its first key, see the design's Security Contexts.
    pub fn id_of(creator: &Id, made: &str, first: &PublicKey) -> anyhow::Result<Id> {
        Id::of(
            &serde_json::json!({ "sc": { "creator": creator.as_str(), "made": made, "key": first.key, "alg": first.alg } }),
        )
    }

    /// The ID of a domain made at made by its founder, see the design's Domains.
    pub fn domain_id_of(made: &str, founder: &Founder) -> anyhow::Result<Id> {
        Id::of(&serde_json::json!({ "domain": {
            "made": made,
            "nonce": founder.nonce,
            "alg": founder.first.alg,
            "key": founder.first.key,
            "next": founder.next,
        } }))
    }

    /// Checks that its ID is the one what it says it is made from gives.
    fn check_id(&self) -> anyhow::Result<()> {
        let id = match (&self.founder, self.root) {
            (Some(founder), true) => Self::domain_id_of(&self.made, founder)?,
            (None, false) => {
                let first = self.keys.get("1").ok_or_else(|| anyhow!("It has no first key"))?;
                Self::id_of(&Id::parse(&self.creator)?, &self.made, first)?
            }
            _ => bail!("Only a domain has a founder"),
        };
        if id.as_str() != self.id {
            bail!("Its ID is not the one what it is made from gives");
        }
        Ok(())
    }

    /// Signs it again, by by, after a change, e.g. to default, which only an Admin of it may make, see Security.
    pub fn sign(&mut self, by: &PrincipalSecret) -> anyhow::Result<()> {
        self.at = Micros::now().to_text();
        sign(self, by)
    }

    /// The hash key of the DataSet of store's domain's SCs' records, in which each is by its ID.
    pub fn hash_key(store: &Store) -> anyhow::Result<String> {
        store.hash_key(SC_KIND, &[])
    }

    pub fn item(&self, store: &Store, origin: &str) -> anyhow::Result<Item> {
        item_of(&Self::hash_key(store)?, &self.id, self, origin)
    }
}

impl MemberRecord {
    /// A new member of sc, member, a principal's or a parent SC's ID, with role, signed by by, which must be an Admin
    /// of sc for it to be valid, see Security.
    pub fn new(sc: &Id, member: &Id, role: Role, by: &PrincipalSecret) -> anyhow::Result<MemberRecord> {
        let now = Micros::now().to_text();
        let mut record = MemberRecord {
            sc: sc.to_string(),
            member: member.to_string(),
            role,
            since: now.clone(),
            at: now,
            removed: false,
            backup: false,
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut record, by)?;
        Ok(record)
    }

    /// The record which removes it, signed by by.
    pub fn removed_by(&self, by: &PrincipalSecret) -> anyhow::Result<MemberRecord> {
        let mut record = MemberRecord {
            at: Micros::now().to_text(),
            removed: true,
            ..self.clone()
        };
        sign(&mut record, by)?;
        Ok(record)
    }

    /// The record which gives it role instead, signed by by.
    pub fn with_role(&self, role: Role, by: &PrincipalSecret) -> anyhow::Result<MemberRecord> {
        let mut record = MemberRecord {
            role,
            at: Micros::now().to_text(),
            removed: false,
            ..self.clone()
        };
        sign(&mut record, by)?;
        Ok(record)
    }

    /// The hash key of the DataSet of the SC sc's memberships in store's domain, in which each is by the member's ID
    /// and when it was made, so that none replaces another.
    pub fn hash_key(store: &Store, sc: &Id) -> anyhow::Result<String> {
        store.hash_key(MEMBER_KIND, &[Part::Id(sc.clone())])
    }

    pub fn item(&self, store: &Store, origin: &str) -> anyhow::Result<Item> {
        item_of(
            &Self::hash_key(store, &Id::parse(&self.sc)?)?,
            &format!("{}#{}", self.member, self.at),
            self,
            origin,
        )
    }
}

impl DelegationRecord {
    /// A new delegation by subject, which signs it, letting actor act for it up to role, which may not be Admin.
    pub fn new(subject: &PrincipalSecret, actor: &Id, role: Role) -> anyhow::Result<DelegationRecord> {
        if role == Role::Admin {
            bail!("A delegation is never of Admin, which needs the subject itself");
        }
        let now = Micros::now().to_text();
        let mut record = DelegationRecord {
            subject: subject.id().to_string(),
            actor: actor.to_string(),
            role,
            since: now.clone(),
            at: now,
            until: None,
            by: String::new(),
            sig: String::new(),
        };
        sign(&mut record, subject)?;
        Ok(record)
    }

    /// The record which ends it, signed by its subject.
    pub fn ended_by(&self, subject: &PrincipalSecret) -> anyhow::Result<DelegationRecord> {
        let now = Micros::now().to_text();
        let mut record = DelegationRecord {
            at: now.clone(),
            until: Some(now),
            ..self.clone()
        };
        sign(&mut record, subject)?;
        Ok(record)
    }

    /// The hash key of the DataSet of subject's delegations in store's domain, in which each is by the actor's ID.
    pub fn hash_key(store: &Store, subject: &Id) -> anyhow::Result<String> {
        store.hash_key(DELEGATION_KIND, &[Part::Id(subject.clone())])
    }

    pub fn item(&self, store: &Store, origin: &str) -> anyhow::Result<Item> {
        item_of(
            &Self::hash_key(store, &Id::parse(&self.subject)?)?,
            &self.actor,
            self,
            origin,
        )
    }

    /// The key it is kept by, among delegations, see Security.
    fn key(&self) -> (String, String) {
        (self.subject.clone(), self.actor.clone())
    }
}

/// The subject of a request, see sign_request_for: present, signing too, or away, its actor acting for it by a
/// delegation.
pub enum Subject<'a> {
    Present(&'a PrincipalSecret),
    Away(&'a Id),
}

/// Who made a request, see Security::check_request: its subject, whose authority it uses, and its actor, which made
/// it, the same for a principal acting for itself; delegated if the subject did not sign, so that what it may do is
/// also limited by the delegation, see Security::role_for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requester {
    pub subject: Id,
    pub actor: Id,
    pub delegated: bool,
}

/// The Authorization header of a request of method for uri, its path and query, with body, at time, in seconds since
/// 1970, by the principal whose secret is by, acting for itself, see the design's "Signed requests".
pub fn sign_request(by: &PrincipalSecret, method: &str, uri: &str, body: &[u8], time: i64) -> String {
    let id = by.id().as_str();
    let signature = by.sign_bytes(request_bytes(method, uri, body, time, id, id).as_bytes());
    format!("{SCHEME}{} {time}:{signature}", by.signer())
}

/// The Authorization header of a request, as sign_request's, made by actor for subject, which signs it too if it is
/// present, see the design's "Subjects and actors".
pub fn sign_request_for(
    subject: Subject,
    actor: &PrincipalSecret,
    method: &str,
    uri: &str,
    body: &[u8],
    time: i64,
) -> String {
    let subject_id = match &subject {
        Subject::Present(secret) => secret.id().clone(),
        Subject::Away(id) => (*id).clone(),
    };
    let bytes = request_bytes(method, uri, body, time, subject_id.as_str(), actor.id().as_str());
    let subject_part = match subject {
        Subject::Present(secret) => format!("{}:{}", secret.signer(), secret.sign_bytes(bytes.as_bytes())),
        Subject::Away(id) => id.to_string(),
    };
    format!(
        "{SCHEME}subject={subject_part}, actor={}:{}, time={time}",
        actor.signer(),
        actor.sign_bytes(bytes.as_bytes())
    )
}

fn request_bytes(method: &str, uri: &str, body: &[u8], time: i64, subject: &str, actor: &str) -> String {
    let body: String = Sha256::digest(body).iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{method} {uri} {time} {body} {subject} {actor}")
}

/// When a member had a role in an SC, from one valid membership record until the next, see Security::load.
#[derive(Clone, Debug)]
struct Span {
    role: Role,
    from: Micros,
    /// When the next record changed or removed it, if one has.
    until: Option<Micros>,
}

impl Span {
    fn covers(&self, at: Micros) -> bool {
        self.from <= at && self.until.is_none_or(|until| at < until)
    }
}

/// A record to judge, see Security::load, in the order they were signed.
enum Event {
    Principal(PrincipalRecord),
    Key(KeyRecord),
    Member(MemberRecord),
}

impl Event {
    fn at(&self) -> &str {
        match self {
            Event::Principal(record) => &record.at,
            Event::Key(record) => &record.at,
            Event::Member(record) => &record.at,
        }
    }

    /// Of records signed at the same time, principals first, then keys, then members, so that what one needs is there.
    fn rank(&self) -> u8 {
        match self {
            Event::Principal(_) => 0,
            Event::Key(_) => 1,
            Event::Member(_) => 2,
        }
    }
}

/// Who may do what in a domain, worked out from its security records, see load, so that a node checks a request, see
/// check_request and role.
#[derive(Debug, Clone)]
pub struct Security {
    domain: Id,
    /// The valid principals, by ID.
    principals: HashMap<String, PrincipalRecord>,
    /// The valid keys, by principal then key ID.
    keys: HashMap<(String, String), KeyRecord>,
    /// The SCs, by ID.
    scs: HashMap<String, ScRecord>,
    /// When each member had its role, by SC then member, in the order of the records which gave it.
    spans: HashMap<String, HashMap<String, Vec<Span>>>,
    /// The latest valid membership record of each member, by SC then member.
    members: HashMap<(String, String), MemberRecord>,
    /// The delegations signed by their subjects, by subject then actor, ended or not.
    delegations: HashMap<(String, String), DelegationRecord>,
    /// The valid statements of what principals are called, by subject, then issuer.
    names: HashMap<String, HashMap<String, String>>,
}

impl Security {
    /// Works out who may do what in store's domain, whose ID is the store's, from its records, see the module
    /// documentation. Only records signed as they should be count: the domain's by its founder, whose first key its ID
    /// is made from, or once changed by an Admin of it; a principal's by an Admin of the domain when it was admitted,
    /// or, for the founder, by itself; a key's by itself; an SC's by its creator, or once changed by an Admin of it; a
    /// member's by an Admin of its SC when it was made; and a delegation's by its subject.
    pub fn load(store: &Store) -> anyhow::Result<Security> {
        let domain = store.domain().clone();
        let mut security = Security {
            domain: domain.clone(),
            principals: HashMap::new(),
            keys: HashMap::new(),
            scs: HashMap::new(),
            spans: HashMap::new(),
            members: HashMap::new(),
            delegations: HashMap::new(),
            names: HashMap::new(),
        };
        // The SCs, kept if their IDs are what they are made from; who signed each is checked once the members are
        // known.
        for item in store.all(&ScRecord::hash_key(store)?)? {
            let Some(attributes) = item.attributes else {
                continue;
            };
            let result = serde_json::from_value::<ScRecord>(attributes)
                .map_err(anyhow::Error::from)
                .and_then(|sc| {
                    if sc.id != item.sk {
                        bail!("It is not the record of its sort key");
                    }
                    sc.check_id()?;
                    if sc.root != (sc.id == domain.as_str()) {
                        bail!("Only the domain's own record is a domain's");
                    }
                    Ok(sc)
                });
            match result {
                Ok(sc) => {
                    security.scs.insert(sc.id.clone(), sc);
                }
                Err(error) => log::warn!("Ignoring the SC record {}, {error:#}", item.sk),
            }
        }
        let root = security
            .scs
            .get(domain.as_str())
            .cloned()
            .ok_or_else(|| anyhow!("The domain {domain}'s record is not known"))?;

        // Each principal the domain lists, its record and its keys.
        let mut events = Vec::new();
        let mut candidates: HashMap<String, PrincipalRecord> = HashMap::new();
        for listed in store.all(&PrincipalRecord::list_hash_key(store)?)? {
            let Ok(id) = Id::parse(&listed.sk) else {
                log::warn!("Ignoring the principal {}, which is not an ID", listed.sk);
                continue;
            };
            for item in store.all(&PrincipalRecord::hash_key(store, &id)?)? {
                let Some(attributes) = item.attributes else {
                    continue;
                };
                if item.sk == RECORD_SK {
                    match serde_json::from_value::<PrincipalRecord>(attributes)
                        .map_err(anyhow::Error::from)
                        .and_then(|principal| {
                            if principal.id != id.as_str() || principal.domain != domain.as_str() {
                                bail!("It is not the record of this domain's principal {id}");
                            }
                            principal.check_id()?;
                            Ok(principal)
                        }) {
                        Ok(principal) => {
                            candidates.insert(principal.id.clone(), principal.clone());
                            events.push(Event::Principal(principal));
                        }
                        Err(error) => log::warn!("Ignoring the principal record {id}, {error:#}"),
                    }
                } else if item.sk.starts_with(KEY_SK) {
                    match serde_json::from_value::<KeyRecord>(attributes) {
                        Ok(key) if key.principal == id.as_str() && item.sk == format!("{KEY_SK}{}", key.id) => {
                            events.push(Event::Key(key))
                        }
                        Ok(_) => log::warn!("Ignoring the key {} of {id}, which is not of its keys", item.sk),
                        Err(error) => log::warn!("Ignoring the key {} of {id}, {error:#}", item.sk),
                    }
                }
            }
        }
        // The founder first, whose principal is valid as the domain's ID is made from its first key.
        let founder = root
            .founder
            .clone()
            .ok_or_else(|| anyhow!("The domain {domain}'s record has no founder"))?;
        let founder_record = candidates
            .get(&root.creator)
            .ok_or_else(|| anyhow!("The domain {domain}'s founder, {}, is not known", root.creator))?;
        if founder_record.first != founder.first || founder_record.next != founder.next {
            bail!("The domain {domain}'s founder is not the one its ID was made from");
        }

        // Each SC's memberships, each a record of its own.
        let sc_ids: Vec<String> = security.scs.keys().cloned().collect();
        for sc in &sc_ids {
            for item in store.all(&MemberRecord::hash_key(store, &Id::parse(sc)?)?)? {
                let Some(attributes) = item.attributes else {
                    continue;
                };
                match serde_json::from_value::<MemberRecord>(attributes) {
                    Ok(member) if member.sc == *sc && item.sk == format!("{}#{}", member.member, member.at) => {
                        events.push(Event::Member(member))
                    }
                    Ok(_) => log::warn!(
                        "Ignoring the member record {} of {sc}, which is not of its keys",
                        item.sk
                    ),
                    Err(error) => log::warn!("Ignoring the member record {} of {sc}, {error:#}", item.sk),
                }
            }
        }

        // Then everything in the order it was signed, each judged by what came before.
        let mut timed = Vec::new();
        for event in events {
            match micros(event.at()) {
                Ok(at) => timed.push((at, event)),
                Err(error) => log::warn!("Ignoring a security record, {error:#}"),
            }
        }
        timed.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.rank().cmp(&y.rank())));
        for (at, event) in timed {
            match event {
                Event::Principal(principal) => security.judge_principal(principal, at, &root),
                Event::Key(key) => security.judge_key(key, &candidates),
                Event::Member(member) => security.judge_member(member, at),
            }
        }

        // An SC changed since it was made must have been signed by an Admin of it.
        let changed: Vec<String> = security.scs.keys().cloned().collect();
        for id in changed {
            let sc = &security.scs[&id];
            let result = (|| -> anyhow::Result<()> {
                let at = micros(&sc.at)?;
                security.verify(sc, at)?;
                let (signer, _) = signer_of(&sc.by)?;
                if signer == sc.creator && sc.at == sc.made {
                    return Ok(());
                }
                if security.role_at(signer, &id, at) != Some(Role::Admin) {
                    bail!("it was changed by {signer}, who was not an Admin of it");
                }
                Ok(())
            })();
            if let Err(error) = result {
                if id == domain.as_str() {
                    bail!("The domain {domain}'s record is not valid, {error:#}");
                }
                log::warn!("Ignoring the SC {id}, {error:#}");
                security.scs.remove(&id);
            }
        }

        // The delegations, signed by their subjects.
        let subjects: Vec<String> = security.principals.keys().cloned().collect();
        for subject in &subjects {
            for item in store.all(&DelegationRecord::hash_key(store, &Id::parse(subject)?)?)? {
                let Some(attributes) = item.attributes else {
                    continue;
                };
                let result = serde_json::from_value::<DelegationRecord>(attributes)
                    .map_err(anyhow::Error::from)
                    .and_then(|delegation| {
                        if delegation.subject != *subject || delegation.actor != item.sk {
                            bail!("It is not of its keys");
                        }
                        let (signer, _) = signer_of(&delegation.by)?;
                        if signer != delegation.subject {
                            bail!("It is signed by {signer}, not its subject");
                        }
                        security.verify(&delegation, micros(&delegation.at)?)?;
                        Ok(delegation)
                    });
                match result {
                    Ok(delegation) => {
                        security.delegations.insert(delegation.key(), delegation);
                    }
                    Err(error) => log::warn!("Ignoring the delegation {} of {subject}, {error:#}", item.sk),
                }
            }
        }

        // What each principal is called in the domain, by itself or by whoever admitted it.
        for subject in &subjects {
            let hk = DirectoryRecord::hash_key(store, &Id::parse(subject)?, &domain)?;
            for item in store.all(&hk)? {
                let Some(attributes) = item.attributes else {
                    continue;
                };
                let result = serde_json::from_value::<DirectoryRecord>(attributes)
                    .map_err(anyhow::Error::from)
                    .and_then(|said| {
                        if said.subject != *subject || item.sk != format!("{}#{}", said.issuer, said.label) {
                            bail!("It is not of its keys");
                        }
                        let (signer, _) = signer_of(&said.by)?;
                        if signer != said.issuer {
                            bail!("It is signed by {signer}, not its issuer");
                        }
                        security.verify(&said, micros(&said.at)?)?;
                        Ok(said)
                    });
                match result {
                    Ok(said) if said.label == NAME_LABEL => {
                        if let Some(name) = said.attributes.get(DISPLAY_NAME).and_then(Value::as_str) {
                            security
                                .names
                                .entry(subject.clone())
                                .or_default()
                                .insert(said.issuer.clone(), name.to_string());
                        }
                    }
                    Ok(_) => {}
                    Err(error) => log::warn!("Ignoring what is said of {subject}, {}, {error:#}", item.sk),
                }
            }
        }
        Ok(security)
    }

    /// Admits a principal, if its record is signed by itself as the domain's founder, or by an Admin of the domain at
    /// at.
    fn judge_principal(&mut self, principal: PrincipalRecord, at: Micros, root: &ScRecord) {
        let result = (|| -> anyhow::Result<()> {
            let (signer, key_id) = signer_of(&principal.by)?;
            if signer == principal.id {
                if principal.id != root.creator {
                    bail!("only the domain's founder signs its own record; another must be admitted by an Admin");
                }
                if key_id != "1" {
                    bail!("the founder's record must be signed by its first key");
                }
                return verify_with(&principal, &verifying_key_of(&principal.first)?);
            }
            self.verify(&principal, at)?;
            if self.role_at(signer, root.id.as_str(), at) != Some(Role::Admin) {
                bail!("{signer}, who admitted it, was not an Admin of the domain then");
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.principals.insert(principal.id.clone(), principal);
            }
            Err(error) => log::warn!("Ignoring the principal {}, {error:#}", principal.id),
        }
    }

    /// Keeps a key, if it is its principal's first, signed by itself, or a later one signed by itself, the one its
    /// predecessor's next committed to.
    fn judge_key(&mut self, key: KeyRecord, candidates: &HashMap<String, PrincipalRecord>) {
        let result = (|| -> anyhow::Result<()> {
            let principal = candidates
                .get(&key.principal)
                .ok_or_else(|| anyhow!("its principal is not known"))?;
            if key.by != format!("{}/{}", key.principal, key.id) {
                bail!("it is not signed by itself");
            }
            if key.id == "1" {
                if key.sign != principal.first || key.next != principal.next {
                    bail!("it is not the first key the principal's ID was made from");
                }
            } else {
                let before = (key.id.parse::<u32>().context("its key ID is not a number")? - 1).to_string();
                let previous = self
                    .keys
                    .get(&(key.principal.clone(), before))
                    .ok_or_else(|| anyhow!("the key before it is not known"))?;
                if next_of(&key.sign, &key.kex)? != previous.next {
                    bail!("it is not the key its predecessor committed to");
                }
            }
            verify_with(&key, &verifying_key_of(&key.sign)?)
        })();
        match result {
            Ok(()) => {
                self.keys.insert((key.principal.clone(), key.id.clone()), key);
            }
            Err(error) => log::warn!("Ignoring the key {} of {}, {error:#}", key.id, key.principal),
        }
    }

    /// Applies a membership record, if it is signed by an Admin of its SC at at.
    fn judge_member(&mut self, member: MemberRecord, at: Micros) {
        let result = (|| -> anyhow::Result<()> {
            if !self.scs.contains_key(&member.sc) {
                bail!("its SC is not known");
            }
            self.verify(&member, at)?;
            let (signer, _) = signer_of(&member.by)?;
            if self.role_at(signer, &member.sc, at) != Some(Role::Admin) {
                bail!("{signer} was not an Admin of it then");
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn!(
                "Ignoring the member record of {} in {} at {}, {error:#}",
                member.member,
                member.sc,
                member.at
            );
            return;
        }
        let spans = self
            .spans
            .entry(member.sc.clone())
            .or_default()
            .entry(member.member.clone())
            .or_default();
        if let Some(last) = spans.last_mut()
            && last.until.is_none()
        {
            last.until = Some(at);
        }
        // A removal leaves an empty span, which covers no time, so that it still says the member was ruled on, e.g.
        // an SC's creator removed, see role_through.
        spans.push(Span {
            role: member.role,
            from: at,
            until: member.removed.then_some(at),
        });
        self.members.insert((member.sc.clone(), member.member.clone()), member);
    }

    /// Checks that record is signed by a valid principal's key, valid at at.
    fn verify(&self, record: &impl Signed, at: Micros) -> anyhow::Result<()> {
        let (principal, key_id) = signer_of(record.by())?;
        verify_with(record, &self.verifying_key(principal, key_id, at)?)
    }

    /// The signing key key_id of the valid principal, if it was valid at at.
    fn verifying_key(&self, principal: &str, key_id: &str, at: Micros) -> anyhow::Result<VerifyingKey> {
        if !self.principals.contains_key(principal) {
            bail!("{principal} is not a principal of the domain");
        }
        let key = self
            .keys
            .get(&(principal.to_string(), key_id.to_string()))
            .ok_or_else(|| anyhow!("The principal {principal} has no key {key_id}"))?;
        if !key.valid_at(at) {
            bail!("The key {key_id} of the principal {principal} was not valid then");
        }
        verifying_key_of(&key.sign)
    }

    /// The principal's role in the SC at at, see the design's "The SC graph": the best of its paths to it, each worth
    /// the lowest role along it, its own membership, the SC's creator's Admin, or through a parent SC.
    fn role_at(&self, principal: &str, sc: &str, at: Micros) -> Option<Role> {
        let mut visited = HashSet::new();
        self.role_through(principal, sc, at, &mut visited)
    }

    fn role_through(&self, principal: &str, sc: &str, at: Micros, visited: &mut HashSet<String>) -> Option<Role> {
        if !visited.insert(sc.to_string()) {
            return None;
        }
        let record = self.scs.get(sc)?;
        let members = self.spans.get(sc);
        let covering = |member: &str| -> Option<Role> {
            members?
                .get(member)?
                .iter()
                .find(|span| span.covers(at))
                .map(|span| span.role)
        };
        // The creator is its first Admin until a membership of its own, e.g. removing it, says otherwise from then on,
        // as co-owners are equals, or until the SC is given a parent SC as an Admin, e.g. the domain, through which its
        // Admins are then its Admins, so that removing one from the domain removes it from every SC under it.
        let creator_ruled = |member: &str| -> bool {
            members
                .and_then(|members| members.get(member))
                .is_some_and(|spans| spans.iter().any(|span| span.from <= at))
        };
        let handed_over = members.is_some_and(|members| {
            members.iter().any(|(member, spans)| {
                self.scs.contains_key(member) && spans.iter().any(|span| span.role == Role::Admin && span.from <= at)
            })
        });
        let mut best =
            (record.creator == principal && !handed_over && !creator_ruled(principal)).then_some(Role::Admin);
        best = best.max(covering(principal));
        if let Some(members) = members {
            for parent in members.keys().filter(|member| self.scs.contains_key(*member)) {
                let Some(role) = covering(parent) else {
                    continue;
                };
                if let Some(in_parent) = self.role_through(principal, parent, at, visited) {
                    best = best.max(Some(in_parent.min(role)));
                }
            }
        }
        visited.remove(sc);
        best
    }

    /// The principal's role in the SC now, if it has one: none for a principal which is not valid.
    pub fn role(&self, principal: &Id, sc: &Id) -> Option<Role> {
        if !self.principals.contains_key(principal.as_str()) {
            return None;
        }
        self.role_at(principal.as_str(), sc.as_str(), Micros::now())
    }

    /// Whether the principal may do what role may in the SC now.
    pub fn allows(&self, principal: &Id, sc: &Id, role: Role) -> bool {
        self.role(principal, sc).is_some_and(|has| has >= role)
    }

    /// The domain's ID.
    pub fn domain_id(&self) -> &Id {
        &self.domain
    }

    /// The domain's record, its root SC's.
    pub fn domain(&self) -> &ScRecord {
        &self.scs[self.domain.as_str()]
    }

    /// The valid SC named name of which parent is a member now, e.g. an app's SC under the domain, if there is one.
    pub fn child_named(&self, parent: &Id, name: &str) -> Option<&ScRecord> {
        let now = Micros::now();
        self.scs.values().find(|sc| {
            sc.name == name
                && self
                    .spans
                    .get(&sc.id)
                    .and_then(|members| members.get(parent.as_str()))
                    .is_some_and(|spans| spans.iter().any(|span| span.covers(now)))
        })
    }

    /// The valid SC at the path of names from the domain, e.g. `["bruce-smart-home", "data"]`, see child_named.
    pub fn sc_path(&self, names: &[&str]) -> Option<&ScRecord> {
        let mut sc = self.domain();
        for name in names {
            sc = self.child_named(&Id::parse(&sc.id).ok()?, name)?;
        }
        Some(sc)
    }

    /// The valid principal of the ID, if it is known.
    pub fn principal(&self, id: &Id) -> Option<&PrincipalRecord> {
        self.principals.get(id.as_str())
    }

    /// The valid principals.
    pub fn principals(&self) -> impl Iterator<Item = &PrincipalRecord> {
        self.principals.values()
    }

    /// What the principal is called in the domain: its own name for itself, or another's, if it has none.
    pub fn display_name(&self, id: &Id) -> Option<&str> {
        let names = self.names.get(id.as_str())?;
        names
            .get(id.as_str())
            .or_else(|| names.values().next())
            .map(String::as_str)
    }

    /// The latest valid membership record of member in the SC, if there is one, removed or not.
    pub fn member(&self, sc: &Id, member: &Id) -> Option<&MemberRecord> {
        self.members.get(&(sc.to_string(), member.to_string()))
    }

    /// The valid delegation by subject to actor, if there is one, ended or not.
    pub fn delegation(&self, subject: &Id, actor: &Id) -> Option<&DelegationRecord> {
        self.delegations.get(&(subject.to_string(), actor.to_string()))
    }

    /// The role, which may not be Admin, to which actor may act for subject now, by a delegation which has not ended.
    fn delegated_role(&self, subject: &Id, actor: &Id) -> Option<Role> {
        let delegation = self.delegation(subject, actor)?;
        if delegation.until.is_some() || !self.principals.contains_key(actor.as_str()) {
            return None;
        }
        Some(delegation.role.min(Role::Write))
    }

    /// What the requester may do in the SC now: its subject's role, and, if delegated, no more than the delegation's.
    pub fn role_for(&self, requester: &Requester, sc: &Id) -> Option<Role> {
        let role = self.role(&requester.subject, sc)?;
        if !requester.delegated {
            return Some(role);
        }
        Some(role.min(self.delegated_role(&requester.subject, &requester.actor)?))
    }

    /// Whether the requester may do what role may in the SC now, see role_for.
    pub fn allows_request(&self, requester: &Requester, sc: &Id, role: Role) -> bool {
        self.role_for(requester, sc).is_some_and(|has| has >= role)
    }

    /// Whether the requester may be given the domain's security records, see the design's "Servers and clients": only
    /// if its subject is an Admin of the domain, as a co-owner is, and it may read, so that a client is never given a
    /// list of members.
    pub fn sees_security_records(&self, requester: &Requester) -> bool {
        self.allows(&requester.subject, &self.domain, Role::Admin)
            && (!requester.delegated || self.delegated_role(&requester.subject, &requester.actor).is_some())
    }

    /// Who made a request of method for uri with body, by its Authorization header, see sign_request and
    /// sign_request_for, now being the node's time in seconds since 1970: its subject and actor, if each signature is
    /// by a key valid now, of a principal the node knows, within REQUEST_WINDOW of now, and, if the subject did not
    /// sign, the actor has a delegation from it which has not ended; what it may do is then for the caller to ask, see
    /// role_for.
    pub fn check_request(
        &self,
        authorization: Option<&str>,
        method: &str,
        uri: &str,
        body: &[u8],
        now: i64,
    ) -> anyhow::Result<Requester> {
        let rest = authorization
            .and_then(|header| header.strip_prefix(SCHEME))
            .ok_or_else(|| anyhow!("The request is not signed"))?;
        let malformed = || anyhow!("The request's signature is not as it should be");
        // The subject's signer and signature, if it signed, its ID, the actor's signer and signature, and the time.
        let (subject_signed, subject_id, actor, time) = if rest.starts_with("subject=") {
            let mut parts: HashMap<&str, &str> = HashMap::new();
            for part in rest.split(", ") {
                let (name, value) = part.split_once('=').ok_or_else(malformed)?;
                parts.insert(name, value);
            }
            let subject = *parts.get("subject").ok_or_else(malformed)?;
            let actor = parts
                .get("actor")
                .and_then(|actor| actor.split_once(':'))
                .ok_or_else(malformed)?;
            let time = *parts.get("time").ok_or_else(malformed)?;
            match subject.split_once(':') {
                Some((by, signature)) => (Some((by, signature)), signer_of(by)?.0, actor, time),
                None => (None, subject, actor, time),
            }
        } else {
            let (by, signed) = rest.split_once(' ').ok_or_else(malformed)?;
            let (time, signature) = signed.split_once(':').ok_or_else(malformed)?;
            let id = signer_of(by)?.0;
            (None, id, (by, signature), time)
        };
        let time: i64 = time.parse().context("The request's time is not a number")?;
        if (now - time).abs() > REQUEST_WINDOW {
            bail!("The request's time, {time}, is not the node's, {now}");
        }
        let (actor_by, actor_signature) = actor;
        let actor_id = signer_of(actor_by)?.0;
        let bytes = request_bytes(method, uri, body, time, subject_id, actor_id);
        let at = Micros(now * 1_000_000);
        let check = |by: &str, signature: &str| -> anyhow::Result<()> {
            let (principal, key_id) = signer_of(by)?;
            if !self.principals.contains_key(principal) {
                bail!("The request is signed by {principal}, who is not known");
            }
            self.verifying_key(principal, key_id, at)?
                .verify_strict(bytes.as_bytes(), &signature_of(signature)?)
                .map_err(|_| anyhow!("The request's signature by {by} is not right"))
        };
        check(actor_by, actor_signature)?;
        let short = !rest.starts_with("subject=");
        if let Some((by, signature)) = subject_signed {
            check(by, signature)?;
        }
        let requester = Requester {
            subject: Id::parse(subject_id)?,
            actor: Id::parse(actor_id)?,
            delegated: !short && subject_signed.is_none(),
        };
        if requester.delegated && self.delegated_role(&requester.subject, &requester.actor).is_none() {
            bail!(
                "{} acts for {}, which has not delegated to it",
                requester.actor,
                requester.subject
            );
        }
        Ok(requester)
    }
}
