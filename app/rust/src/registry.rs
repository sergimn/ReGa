//! The clouds the app knows about. [`connect`] signs in to each one with the
//! credentials the user saved in ReGa; a cloud without any shows sample data
//! until the user enters some ([`sign_in`]).
//!
//! Credentials only ever come from what the user entered in the app, kept in
//! the system keyring. Nothing is picked up from environment variables,
//! `~/.aws` or any other cloud CLI's configuration, even when the computer
//! has credentials there.

use std::sync::{Arc, LazyLock, RwLock};
use std::thread;
use std::time::Duration;

use futures::channel::oneshot;
use futures::lock::Mutex;
use rega_core::aws::{AwsConnector, ConnectOptions};
use rega_core::credentials::{CredentialField, CredentialStore, Credentials, KeyringStore};
use rega_core::latency::RttCache;
use rega_core::mock::MockProvider;
use rega_core::{CloudProvider, Connector, Region};

/// Setting this to anything makes the app use sample data and leave the
/// keyring alone (for screenshots and the integration test).
pub const DEMO_ENV: &str = "REGA_DEMO";

pub enum State {
    /// Sample data, with the reason the real account isn't used.
    Demo {
        provider: MockProvider,
        reason: String,
    },
    Live {
        provider: Box<dyn CloudProvider>,
        rtt: RttCache,
    },
}

/// One cloud, signed in or showing sample data.
pub struct Entry {
    pub state: State,
    /// What the user fills in to sign in.
    pub fields: Vec<CredentialField>,
    /// The values of the fields that aren't secret, when credentials are
    /// saved, to start the form from. `None` when nothing is saved.
    pub saved: Option<Vec<(String, String)>>,
    /// Something the user should know about a signed-in cloud, such as its
    /// credentials not being saved.
    pub warning: Option<String>,
}

impl Entry {
    /// AWS sample data, with nothing saved.
    #[cfg(test)]
    pub fn demo(reason: impl Into<String>) -> Self {
        Entry {
            state: State::Demo {
                provider: MockProvider::demo_aws(),
                reason: reason.into(),
            },
            fields: rega_core::aws::credential_fields(),
            saved: None,
            warning: None,
        }
    }

    pub fn provider(&self) -> &dyn CloudProvider {
        match &self.state {
            State::Demo { provider, .. } => provider,
            State::Live { provider, .. } => provider.as_ref(),
        }
    }

    /// Why sample data is shown, or `None` for a real account.
    pub fn demo_reason(&self) -> Option<&str> {
        match &self.state {
            State::Demo { reason, .. } => Some(reason),
            State::Live { .. } => None,
        }
    }

    pub fn rtt(&self, region_id: &str) -> Option<Duration> {
        match &self.state {
            State::Demo { provider, .. } => provider.simulated_rtt(region_id),
            State::Live { rtt, .. } => rtt.get(region_id),
        }
    }

    /// Measures latency to `regions` off the async executor and resolves
    /// when every probe is done, with `true` if any region was measured.
    /// Demo data never changes, so it's `false`.
    pub async fn probe(&self, regions: Vec<Region>) -> bool {
        let State::Live { rtt, .. } = &self.state else {
            return false;
        };
        let (done, finished) = oneshot::channel();
        let handles = rtt.measure_in_background(regions);
        thread::spawn(move || {
            // Join every probe (no short-circuit) so this resolves only
            // once all of them are done.
            let results: Vec<bool> = handles
                .into_iter()
                .map(|h| h.join().unwrap_or(false))
                .collect();
            let measured = results.contains(&true);
            let _ = done.send(measured);
        });
        finished.await.unwrap_or(false)
    }
}

/// The non-secret values of `credentials`, to show in the form again.
fn visible(fields: &[CredentialField], credentials: &Credentials) -> Vec<(String, String)> {
    fields
        .iter()
        .filter(|f| !f.secret)
        .filter_map(|f| Some((f.id.clone(), credentials.get(&f.id)?.to_string())))
        .collect()
}

/// The clouds, where their credentials are kept, and what each one shows.
pub struct Registry {
    connectors: Vec<Arc<dyn Connector>>,
    store: Arc<dyn CredentialStore>,
    /// Sample data whatever is saved ([`DEMO_ENV`]).
    demo_only: bool,
    entries: RwLock<Vec<Arc<Entry>>>,
    /// One sign-in, sign-out or reconnect at a time, so the keyring and
    /// the entries can't end up telling different stories.
    changing: Mutex<()>,
}

impl Registry {
    pub fn new(
        connectors: Vec<Arc<dyn Connector>>,
        store: Arc<dyn CredentialStore>,
        demo_only: bool,
    ) -> Self {
        Self {
            connectors,
            store,
            demo_only,
            entries: RwLock::new(Vec::new()),
            changing: Mutex::new(()),
        }
    }

    fn connector(&self, provider_id: &str) -> Result<&Arc<dyn Connector>, String> {
        self.connectors
            .iter()
            .find(|c| c.id() == provider_id)
            .ok_or_else(|| format!("unknown provider {provider_id}"))
    }

    fn demo(connector: &dyn Connector, reason: String) -> Entry {
        Entry {
            state: State::Demo {
                provider: connector.demo(),
                reason,
            },
            fields: connector.credential_fields(),
            saved: None,
            warning: None,
        }
    }

    fn live(connector: &dyn Connector, provider: Box<dyn CloudProvider>) -> Entry {
        Entry {
            state: State::Live {
                provider,
                rtt: RttCache::default(),
            },
            fields: connector.credential_fields(),
            saved: None,
            warning: None,
        }
    }

    /// The cloud signed in with what's saved for it, or sample data saying
    /// why not.
    async fn open(&self, connector: &dyn Connector) -> Entry {
        let name = connector.display_name();
        if self.demo_only {
            return Self::demo(connector, format!("{DEMO_ENV} is set"));
        }
        let credentials = match self.store.load(connector.id()).await {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                return Self::demo(
                    connector,
                    format!("Enter your {name} credentials to use your account."),
                )
            }
            Err(e) => {
                return Self::demo(
                    connector,
                    format!("Couldn't read the saved {name} credentials: {e}"),
                )
            }
        };
        let fields = connector.credential_fields();
        let saved = Some(visible(&fields, &credentials));
        let entry = match connector.connect(&credentials).await {
            Ok(provider) => Self::live(connector, provider),
            Err(e) => Self::demo(
                connector,
                format!("Couldn't sign in to {name} with the saved credentials: {e}"),
            ),
        };
        Entry { saved, ..entry }
    }

    fn replace(&self, entry: Entry) -> Arc<Entry> {
        let entry = Arc::new(entry);
        let mut entries = self.entries.write().unwrap();
        let id = entry.provider().id().to_string();
        match entries.iter_mut().find(|e| e.provider().id() == id) {
            Some(slot) => *slot = entry.clone(),
            None => entries.push(entry.clone()),
        }
        entry
    }

    /// Signs in to every cloud, replacing what was there.
    pub async fn connect(&self) -> Vec<Arc<Entry>> {
        let _one_at_a_time = self.changing.lock().await;
        let mut entries = Vec::new();
        for connector in &self.connectors {
            entries.push(self.open(connector.as_ref()).await);
        }
        self.set(entries);
        self.entries()
    }

    /// Signs in to `provider_id` with `credentials` and, once the cloud has
    /// accepted them, saves them in place of what was saved before. Nothing
    /// changes when the cloud refuses them. If they can't be saved, the
    /// cloud stays signed in for now, with a warning.
    pub async fn sign_in(
        &self,
        provider_id: &str,
        credentials: Credentials,
    ) -> Result<Arc<Entry>, String> {
        let _one_at_a_time = self.changing.lock().await;
        let connector = self.connector(provider_id)?;
        let name = connector.display_name();
        if self.demo_only {
            return Err(format!("{DEMO_ENV} is set, so ReGa only shows sample data"));
        }
        let fields = connector.credential_fields();
        credentials.check(&fields).map_err(|e| e.to_string())?;
        let provider = connector
            .connect(&credentials)
            .await
            .map_err(|e| format!("Couldn't sign in to {name}: {e}"))?;
        let mut entry = Self::live(connector.as_ref(), provider);
        match self.store.save(provider_id, &credentials).await {
            Ok(()) => entry.saved = Some(visible(&fields, &credentials)),
            Err(e) => {
                entry.warning = Some(format!(
                    "Signed in, but the credentials couldn't be saved, so ReGa \
                     will ask for them again next time: {e}"
                ))
            }
        }
        Ok(self.replace(entry))
    }

    /// Forgets the credentials saved for `provider_id` and goes back to
    /// sample data. Machines in the account are left alone.
    pub async fn sign_out(&self, provider_id: &str) -> Result<Arc<Entry>, String> {
        let _one_at_a_time = self.changing.lock().await;
        let connector = self.connector(provider_id)?;
        if !self.demo_only {
            self.store
                .delete(provider_id)
                .await
                .map_err(|e| format!("Couldn't forget the credentials: {e}"))?;
        }
        let reason = format!(
            "Signed out. Enter your {} credentials to use your account.",
            connector.display_name()
        );
        Ok(self.replace(Self::demo(connector.as_ref(), reason)))
    }

    pub fn set(&self, entries: Vec<Entry>) {
        *self.entries.write().unwrap() = entries.into_iter().map(Arc::new).collect();
    }

    pub fn entries(&self) -> Vec<Arc<Entry>> {
        self.entries.read().unwrap().clone()
    }

    pub fn entry(&self, provider_id: &str) -> Result<Arc<Entry>, String> {
        self.entries()
            .into_iter()
            .find(|e| e.provider().id() == provider_id)
            .ok_or_else(|| format!("unknown provider {provider_id}"))
    }
}

/// The app's registry: AWS, with credentials in the system keyring.
static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    Registry::new(
        vec![Arc::new(AwsConnector {
            options: ConnectOptions::from_env(),
        })],
        Arc::new(KeyringStore),
        std::env::var_os(DEMO_ENV).is_some(),
    )
});

/// Signs in to every provider with its saved credentials, replacing what
/// was there.
pub async fn connect() -> Vec<Arc<Entry>> {
    REGISTRY.connect().await
}

pub async fn sign_in(provider_id: &str, credentials: Credentials) -> Result<Arc<Entry>, String> {
    REGISTRY.sign_in(provider_id, credentials).await
}

pub async fn sign_out(provider_id: &str) -> Result<Arc<Entry>, String> {
    REGISTRY.sign_out(provider_id).await
}

/// Replaces every entry, for tests that want one.
#[cfg(test)]
pub fn set(entries: Vec<Entry>) {
    REGISTRY.set(entries)
}

pub fn entry(provider_id: &str) -> Result<Arc<Entry>, String> {
    REGISTRY.entry(provider_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rega_core::credentials::MemoryStore;
    use rega_core::{ProviderError, ProviderResult};
    use std::sync::Mutex as StdMutex;

    /// A cloud that accepts one access key and records every sign-in.
    struct FakeCloud {
        accepted: String,
        tries: StdMutex<Vec<Credentials>>,
    }

    impl FakeCloud {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                accepted: format!("typed-{}", std::process::id()),
                tries: StdMutex::new(Vec::new()),
            })
        }

        fn good(&self) -> Credentials {
            Credentials::new([
                ("access_key_id", "typed-key-id"),
                ("secret_access_key", self.accepted.as_str()),
            ])
        }

        fn bad(&self) -> Credentials {
            Credentials::new([
                ("access_key_id", "typed-key-id"),
                ("secret_access_key", "wrong"),
            ])
        }
    }

    #[async_trait]
    impl Connector for FakeCloud {
        fn id(&self) -> &str {
            "aws"
        }

        fn display_name(&self) -> &str {
            "AWS"
        }

        fn credential_fields(&self) -> Vec<CredentialField> {
            rega_core::aws::credential_fields()
        }

        async fn connect(
            &self,
            credentials: &Credentials,
        ) -> ProviderResult<Box<dyn CloudProvider>> {
            self.tries.lock().unwrap().push(credentials.clone());
            if credentials.get("secret_access_key") == Some(self.accepted.as_str()) {
                // A real account, as far as the registry can tell.
                Ok(Box::new(MockProvider::demo_aws()))
            } else {
                Err(ProviderError::Cloud("InvalidClientTokenId".into()))
            }
        }

        fn demo(&self) -> MockProvider {
            MockProvider::demo_aws()
        }
    }

    fn registry(cloud: &Arc<FakeCloud>, store: &Arc<MemoryStore>, demo_only: bool) -> Registry {
        Registry::new(vec![cloud.clone()], store.clone(), demo_only)
    }

    fn live(e: &Entry) -> bool {
        matches!(e.state, State::Live { .. })
    }

    #[tokio::test]
    async fn nothing_saved_means_sample_data_and_no_sign_in() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        let entries = registry(&cloud, &store, false).connect().await;
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(
            e.demo_reason(),
            Some("Enter your AWS credentials to use your account.")
        );
        assert!(e.saved.is_none());
        assert_eq!(e.fields, rega_core::aws::credential_fields());
        // Not even an attempt with whatever the computer might have.
        assert!(cloud.tries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn saved_credentials_are_the_only_ones_used() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        store.save("aws", &cloud.good()).await.unwrap();
        let entries = registry(&cloud, &store, false).connect().await;
        assert!(live(&entries[0]));
        assert_eq!(*cloud.tries.lock().unwrap(), vec![cloud.good()]);
        // The form starts from the key ID, never the secret.
        assert_eq!(
            entries[0].saved,
            Some(vec![("access_key_id".into(), "typed-key-id".into())])
        );
    }

    #[tokio::test]
    async fn saved_credentials_that_stop_working_say_so() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        store.save("aws", &cloud.bad()).await.unwrap();
        let entries = registry(&cloud, &store, false).connect().await;
        let e = &entries[0];
        assert_eq!(
            e.demo_reason(),
            Some("Couldn't sign in to AWS with the saved credentials: InvalidClientTokenId")
        );
        // Still saved, so the page offers to change them or sign out.
        assert!(e.saved.is_some());
    }

    #[tokio::test]
    async fn a_keyring_that_cant_be_read_says_so() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        *store.failure.lock().unwrap() = Some("no Secret Service".into());
        let entries = registry(&cloud, &store, false).connect().await;
        assert_eq!(
            entries[0].demo_reason(),
            Some("Couldn't read the saved AWS credentials: the system keyring: no Secret Service")
        );
        assert!(cloud.tries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn signing_in_checks_then_saves() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        let reg = registry(&cloud, &store, false);
        reg.connect().await;

        let err = reg.sign_in("aws", cloud.bad()).await.err().unwrap();
        assert_eq!(err, "Couldn't sign in to AWS: InvalidClientTokenId");
        assert_eq!(store.load("aws").await, Ok(None));
        assert!(!live(&reg.entry("aws").unwrap()));

        let e = reg.sign_in("aws", cloud.good()).await.unwrap();
        assert!(live(&e));
        assert!(e.warning.is_none());
        assert_eq!(store.load("aws").await, Ok(Some(cloud.good())));
        assert!(live(&reg.entry("aws").unwrap()));
        assert_eq!(reg.entries().len(), 1);
    }

    #[tokio::test]
    async fn a_half_filled_form_never_reaches_the_cloud() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        let reg = registry(&cloud, &store, false);
        let only_id = Credentials::new([("access_key_id", "typed-key-id")]);
        let err = reg.sign_in("aws", only_id).await.err().unwrap();
        assert_eq!(err, "enter the Secret access key");
        assert!(cloud.tries.lock().unwrap().is_empty());
        assert_eq!(
            reg.sign_in("gcp", cloud.good()).await.err().unwrap(),
            "unknown provider gcp"
        );
    }

    #[tokio::test]
    async fn credentials_that_cant_be_saved_still_sign_in_for_now() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        let reg = registry(&cloud, &store, false);
        *store.failure.lock().unwrap() = Some("locked".into());
        let e = reg.sign_in("aws", cloud.good()).await.unwrap();
        assert!(live(&e));
        assert!(e.saved.is_none());
        assert_eq!(
            e.warning.as_deref(),
            Some(
                "Signed in, but the credentials couldn't be saved, so ReGa will ask \
                 for them again next time: the system keyring: locked"
            )
        );
    }

    #[tokio::test]
    async fn signing_out_forgets_the_credentials() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        store.save("aws", &cloud.good()).await.unwrap();
        let reg = registry(&cloud, &store, false);
        reg.connect().await;

        let e = reg.sign_out("aws").await.unwrap();
        assert!(!live(&e));
        assert!(e.saved.is_none());
        assert_eq!(store.load("aws").await, Ok(None));
        assert!(!live(&reg.entry("aws").unwrap()));
        // The next start doesn't sign in either.
        reg.connect().await;
        assert!(!live(&reg.entry("aws").unwrap()));
        assert_eq!(cloud.tries.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn demo_only_leaves_the_keyring_alone() {
        let cloud = FakeCloud::new();
        let store = Arc::new(MemoryStore::default());
        // Any use of the store would fail.
        *store.failure.lock().unwrap() = Some("touched".into());
        let reg = registry(&cloud, &store, true);
        let entries = reg.connect().await;
        assert_eq!(entries[0].demo_reason(), Some("REGA_DEMO is set"));
        let err = reg.sign_in("aws", cloud.good()).await.err().unwrap();
        assert_eq!(err, "REGA_DEMO is set, so ReGa only shows sample data");
        assert!(reg.sign_out("aws").await.is_ok());
        assert!(cloud.tries.lock().unwrap().is_empty());
    }
}
