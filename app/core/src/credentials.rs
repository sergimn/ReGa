//! Cloud credentials the user typed into ReGa, and where they are kept.
//!
//! ReGa never picks credentials up on its own: no environment variables, no
//! `~/.aws` or other CLI configuration, no instance metadata. A cloud is
//! used only with what the user entered in the app, even when the computer
//! has other credentials, so the wrong account is never used without anyone
//! noticing. What they enter is kept in the system keyring
//! ([`KeyringStore`]).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::provider::{ProviderError, ProviderResult};

/// One value a cloud needs to sign in, such as an access key ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialField {
    /// What the provider reads the value by, such as `access_key_id`.
    pub id: String,
    /// What the form calls it, such as `Access key ID`.
    pub label: String,
    /// Hidden while typed, and never shown again once saved.
    pub secret: bool,
}

impl CredentialField {
    pub fn new(id: &str, label: &str, secret: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            secret,
        }
    }
}

/// What the user entered for one cloud, by field id. `Debug` leaves the
/// values out.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials(BTreeMap<String, String>);

impl Credentials {
    /// Values as typed, without the spaces a paste tends to bring along.
    /// Empty values are left out.
    pub fn new<K: Into<String>, V: AsRef<str>>(values: impl IntoIterator<Item = (K, V)>) -> Self {
        Self(
            values
                .into_iter()
                .map(|(k, v)| (k.into(), v.as_ref().trim().to_string()))
                .filter(|(_, v)| !v.is_empty())
                .collect(),
        )
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        self.0.get(id).map(String::as_str)
    }

    /// Every one of `fields` is filled in, and nothing else is.
    pub fn check(&self, fields: &[CredentialField]) -> ProviderResult<()> {
        if let Some(missing) = fields.iter().find(|f| self.get(&f.id).is_none()) {
            return Err(ProviderError::InvalidState(format!(
                "enter the {}",
                missing.label
            )));
        }
        if let Some(unknown) = self.0.keys().find(|k| !fields.iter().any(|f| &f.id == *k)) {
            return Err(ProviderError::InvalidState(format!(
                "unexpected credential {unknown}"
            )));
        }
        Ok(())
    }

    fn to_json(&self) -> String {
        serde_json::to_string(&self.0).expect("a map of strings serialises")
    }

    fn from_json(json: &[u8]) -> ProviderResult<Self> {
        // Not serde's message: it quotes the value it choked on, which
        // could be a secret.
        serde_json::from_slice(json)
            .map(Self)
            .map_err(|_| keyring_error("the saved credentials aren't in a form ReGa can read"))
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// Where the credentials of each cloud are kept, by provider id.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// `None` when nothing is saved for `provider_id`.
    async fn load(&self, provider_id: &str) -> ProviderResult<Option<Credentials>>;

    /// Replaces what was saved for `provider_id`.
    async fn save(&self, provider_id: &str, credentials: &Credentials) -> ProviderResult<()>;

    /// Succeeds when nothing was saved.
    async fn delete(&self, provider_id: &str) -> ProviderResult<()>;
}

fn keyring_error(e: impl fmt::Display) -> ProviderError {
    ProviderError::Keyring(e.to_string())
}

const APP_ID: &str = "io.github.sergimn.rega";
const SCHEMA: &str = "io.github.sergimn.rega.Credentials";

/// The system keyring. Outside a sandbox that is the desktop's Secret
/// Service (GNOME Keyring, KWallet); inside the Flatpak, a file in the
/// app's data folder encrypted with a key the Secret portal hands out, so
/// the app needs no access to other apps' secrets. One item per cloud,
/// holding its values as JSON.
pub struct KeyringStore;

impl KeyringStore {
    fn attributes(provider_id: &str) -> [(&str, &str); 3] {
        [
            (oo7::XDG_SCHEMA_ATTRIBUTE, SCHEMA),
            ("application", APP_ID),
            ("provider", provider_id),
        ]
    }

    async fn open() -> ProviderResult<oo7::Keyring> {
        let keyring = oo7::Keyring::new().await.map_err(keyring_error)?;
        keyring.unlock().await.map_err(keyring_error)?;
        Ok(keyring)
    }
}

#[async_trait]
impl CredentialStore for KeyringStore {
    async fn load(&self, provider_id: &str) -> ProviderResult<Option<Credentials>> {
        let keyring = Self::open().await?;
        let items = keyring
            .search_items(&Self::attributes(provider_id))
            .await
            .map_err(keyring_error)?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        item.unlock().await.map_err(keyring_error)?;
        let secret = item.secret().await.map_err(keyring_error)?;
        Credentials::from_json(secret.as_bytes()).map(Some)
    }

    async fn save(&self, provider_id: &str, credentials: &Credentials) -> ProviderResult<()> {
        let keyring = Self::open().await?;
        let attributes = Self::attributes(provider_id);
        // Whatever was there goes first, so there's only ever one item to
        // read back, however the keyring treats `replace`.
        keyring.delete(&attributes).await.map_err(keyring_error)?;
        keyring
            .create_item(
                &format!("ReGa: {provider_id} credentials"),
                &attributes,
                oo7::Secret::text(credentials.to_json()),
                true,
            )
            .await
            .map_err(keyring_error)
    }

    async fn delete(&self, provider_id: &str) -> ProviderResult<()> {
        let keyring = Self::open().await?;
        keyring
            .delete(&Self::attributes(provider_id))
            .await
            .map_err(keyring_error)
    }
}

/// Keeps credentials in memory only, for tests and demo runs.
#[derive(Default)]
pub struct MemoryStore {
    saved: Mutex<HashMap<String, String>>,
    /// Every call fails with this, when set.
    pub failure: Mutex<Option<String>>,
    /// Saving fails with this, when set, while the rest works.
    pub save_failure: Mutex<Option<String>>,
}

fn fail_if(failure: &Mutex<Option<String>>) -> ProviderResult<()> {
    match failure.lock().unwrap().clone() {
        Some(e) => Err(keyring_error(e)),
        None => Ok(()),
    }
}

#[async_trait]
impl CredentialStore for MemoryStore {
    async fn load(&self, provider_id: &str) -> ProviderResult<Option<Credentials>> {
        fail_if(&self.failure)?;
        let saved = self.saved.lock().unwrap().get(provider_id).cloned();
        saved
            .map(|json| Credentials::from_json(json.as_bytes()))
            .transpose()
    }

    async fn save(&self, provider_id: &str, credentials: &Credentials) -> ProviderResult<()> {
        fail_if(&self.failure)?;
        fail_if(&self.save_failure)?;
        self.saved
            .lock()
            .unwrap()
            .insert(provider_id.into(), credentials.to_json());
        Ok(())
    }

    async fn delete(&self, provider_id: &str) -> ProviderResult<()> {
        fail_if(&self.failure)?;
        self.saved.lock().unwrap().remove(provider_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<CredentialField> {
        vec![
            CredentialField::new("id", "Key ID", false),
            CredentialField::new("secret", "Secret", true),
        ]
    }

    #[test]
    fn values_are_trimmed_and_blanks_dropped() {
        let c = Credentials::new([("id", "  typed-id \n"), ("secret", "   ")]);
        assert_eq!(c.get("id"), Some("typed-id"));
        assert_eq!(c.get("secret"), None);
    }

    #[test]
    fn every_field_must_be_filled_in() {
        let c = Credentials::new([("id", "typed-id")]);
        assert_eq!(
            c.check(&fields()),
            Err(ProviderError::InvalidState("enter the Secret".into()))
        );
        let c = Credentials::new([("id", "a"), ("secret", "b"), ("region", "c")]);
        assert_eq!(
            c.check(&fields()),
            Err(ProviderError::InvalidState(
                "unexpected credential region".into()
            ))
        );
        let c = Credentials::new([("id", "a"), ("secret", "b")]);
        assert_eq!(c.check(&fields()), Ok(()));
    }

    #[test]
    fn debug_never_shows_the_values() {
        let secret = format!("typed-{}", std::process::id());
        let c = Credentials::new([("id", "typed-id"), ("secret", secret.as_str())]);
        let shown = format!("{c:?}");
        assert_eq!(shown, r#"{"id", "secret"}"#);
        assert!(!shown.contains(&secret));
    }

    /// Saves, reads back and forgets through the real system keyring, over
    /// the session's Secret Service. It writes to that keyring, so it only
    /// runs with `REGA_TEST_KEYRING=1` (CI starts a throwaway GNOME Keyring).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_system_keyring_round_trips() {
        if std::env::var_os("REGA_TEST_KEYRING").is_none() {
            eprintln!("skipped: set REGA_TEST_KEYRING=1 to use the session's keyring");
            return;
        }
        let store = KeyringStore;
        let id = format!("test-{}", std::process::id());
        assert_eq!(store.load(&id).await, Ok(None));
        let first = Credentials::new([("id", "typed-id"), ("secret", "first")]);
        store.save(&id, &first).await.unwrap();
        assert_eq!(store.load(&id).await, Ok(Some(first)));
        // Saving again replaces what was there.
        let second = Credentials::new([("id", "typed-id"), ("secret", "second")]);
        store.save(&id, &second).await.unwrap();
        assert_eq!(store.load(&id).await, Ok(Some(second)));
        // Each provider has its own item.
        assert_eq!(store.load(&format!("{id}-other")).await, Ok(None));
        store.delete(&id).await.unwrap();
        assert_eq!(store.load(&id).await, Ok(None));
        store.delete(&id).await.unwrap();
    }

    #[test]
    fn an_unreadable_item_never_shows_its_contents() {
        let err = Credentials::from_json(br#"{"secret": ["typed-secret"]}"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the system keyring: the saved credentials aren't in a form ReGa can read"
        );
    }

    #[tokio::test]
    async fn the_memory_store_round_trips() {
        let store = MemoryStore::default();
        assert_eq!(store.load("aws").await, Ok(None));
        let c = Credentials::new([("id", "a"), ("secret", "b")]);
        store.save("aws", &c).await.unwrap();
        assert_eq!(store.load("aws").await, Ok(Some(c)));
        assert_eq!(store.load("gcp").await, Ok(None));
        store.delete("aws").await.unwrap();
        assert_eq!(store.load("aws").await, Ok(None));
        store.delete("aws").await.unwrap();
    }
}
