use crate::credentials;
use crate::locales;
use crate::logging::{self, Event, Reason};
use crate::models::{Account, AccountKey, RankInfo, RankProvenance, TftRank};
use crate::rank_fetcher::RankProvider;
use crate::utils::{accounts_file, region_display, sort_accounts, REGION_MAP};
use serde::{Deserialize, Serialize};
use serde::ser::{SerializeSeq, Serializer as _};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

pub const KEYRING_SERVICE: &str = credentials::SERVICE;

pub type ManagerResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Everything an explicit account edit may change. A `None` or empty password
/// keeps the stored credential, so an untouched field cannot wipe it.
#[derive(Clone, Debug)]
pub struct AccountChanges {
    pub name: String,
    pub description: String,
    pub locale: Option<String>,
    pub skip_profile_update: bool,
    pub password: Option<String>,
}

/// One account's admission to a rank refresh. The private generation makes a
/// late result invalid once the account is deleted, renamed, or refreshed
/// again.
pub struct RankJob {
    pub account: Account,
    generation: u64,
}

pub struct AddAccountsResult {
    pub added: Vec<Account>,
    pub skipped_indices: Vec<usize>,
}

// Keep credential failures testable without touching the user's keyring.
trait PasswordStore {
    fn get(&self, key: &str) -> ManagerResult<Option<String>>;
    fn set(&self, key: &str, password: &str) -> ManagerResult<()>;
    fn delete(&self, key: &str) -> ManagerResult<()>;

    fn snapshot(&self, key: &str) -> ManagerResult<credentials::PasswordSnapshot> {
        Ok(credentials::PasswordSnapshot {
            primary: self.get(key)?,
            legacy: None,
        })
    }

    fn restore(&self, key: &str, snapshot: credentials::PasswordSnapshot) -> ManagerResult<()> {
        match snapshot.primary {
            Some(password) => self.set(key, &password),
            None => self.delete(key),
        }
    }
}

struct NativePasswords<'a>(&'a str);

impl PasswordStore for NativePasswords<'_> {
    fn get(&self, key: &str) -> ManagerResult<Option<String>> {
        credentials::get_password(self.0, key)
    }

    fn set(&self, key: &str, password: &str) -> ManagerResult<()> {
        credentials::set_password(self.0, key, password)
    }

    fn delete(&self, key: &str) -> ManagerResult<()> {
        credentials::delete_password(self.0, key)
    }

    fn snapshot(&self, key: &str) -> ManagerResult<credentials::PasswordSnapshot> {
        credentials::snapshot_password(self.0, key)
    }

    fn restore(&self, key: &str, snapshot: credentials::PasswordSnapshot) -> ManagerResult<()> {
        credentials::restore_password(self.0, key, snapshot)
    }
}

pub struct AccountManager {
    pub accounts: Vec<Account>,
    pub accounts_file: PathBuf,
    pub rank_fetcher: Arc<dyn RankProvider>,
    /// Set when the stored file could not be read or parsed. Persistent
    /// mutations are refused until an explicit reload succeeds, so a partial
    /// in-memory list can never replace a file we failed to understand.
    load_failed: bool,
    rank_generation: u64,
    rank_jobs: HashMap<AccountKey, u64>,
}

impl AccountManager {
    pub fn new(rank_fetcher: Arc<dyn RankProvider>) -> ManagerResult<Self> {
        Ok(Self {
            accounts: Vec::new(),
            accounts_file: accounts_file().inspect_err(|error| {
                logging::record(Event::DataDirectoryFailed, Reason::from_error(error));
            })?,
            rank_fetcher,
            load_failed: false,
            rank_generation: 0,
            rank_jobs: HashMap::new(),
        })
    }

    pub fn with_path(path: impl Into<PathBuf>, rank_fetcher: Arc<dyn RankProvider>) -> Self {
        Self {
            accounts: Vec::new(),
            accounts_file: path.into(),
            rank_fetcher,
            load_failed: false,
            rank_generation: 0,
            rank_jobs: HashMap::new(),
        }
    }

    pub fn load_accounts(&mut self) -> ManagerResult<()> {
        self.load_accounts_with_service(KEYRING_SERVICE)
    }

    fn load_accounts_with_service(&mut self, service: &str) -> ManagerResult<()> {
        let contents = match std::fs::read(&self.accounts_file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A file that has never existed is a new, empty, writable store.
                self.accounts.clear();
                self.rank_jobs.clear();
                self.load_failed = false;
                return Ok(());
            }
            Err(error) => {
                logging::record(Event::AccountsLoadFailed, Reason::from_error(&error));
                // Reading failed, so nothing here can honestly be called a
                // backup. Keep the old memory and refuse later writes.
                self.load_failed = true;
                return Err(Box::new(error));
            }
        };
        let mut loaded: Vec<Account> = match serde_json::from_slice(&contents) {
            Ok(loaded) => loaded,
            Err(error) => {
                logging::record(Event::AccountsLoadFailed, Reason::from_error(&error));
                // Damage is visible only after a successful read: preserve the
                // readable file under a name that cannot replace another backup.
                let _ = self.back_up_corrupt_accounts(&contents);
                self.load_failed = true;
                return Err(Box::new(error));
            }
        };
        for account in &mut loaded {
            if account.region_display.is_empty() {
                account.region_display = region_display(&account.region);
            }
            account.password = credentials::load_password_with_migration(
                service,
                &format!("{}:{}", account.region, account.account_id),
            )
            .ok()
            .flatten()
            .unwrap_or_default();
        }
        self.accounts = loaded;
        self.load_failed = false;
        self.rank_jobs.clear();
        sort_accounts(&mut self.accounts);
        Ok(())
    }

    /// Copy a readable but unparseable file beside itself under the first free
    /// `.corrupt[.N]` name. An existing file is never replaced.
    fn back_up_corrupt_accounts(&self, contents: &[u8]) -> ManagerResult<PathBuf> {
        let parent = self
            .accounts_file
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let Some(name) = self.accounts_file.file_name() else {
            return Err(std::io::Error::other("accounts file has no name").into());
        };
        for suffix in 0..=u32::MAX {
            let candidate = if suffix == 0 {
                format!("{}.corrupt", name.to_string_lossy())
            } else {
                format!("{}.corrupt.{suffix}", name.to_string_lossy())
            };
            let candidate = parent.join(candidate);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(contents).and_then(|()| file.sync_all()) {
                        drop(file);
                        let _ = std::fs::remove_file(&candidate);
                        return Err(Box::new(error));
                    }
                    return Ok(candidate);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Box::new(error)),
            }
        }
        Err(std::io::Error::other("no free backup name").into())
    }

    pub fn save_accounts(&self) -> ManagerResult<()> {
        self.persist_accounts(self.prepare_accounts(&self.accounts)?)
    }

    fn ensure_loaded(&self) -> ManagerResult<()> {
        if self.load_failed {
            return Err(std::io::Error::other(
                "stored accounts have not loaded successfully",
            )
            .into());
        }
        Ok(())
    }

    fn prepare_accounts<'a>(
        &self,
        accounts: impl IntoIterator<Item = &'a Account>,
    ) -> ManagerResult<tempfile::NamedTempFile> {
        let result = (|| -> ManagerResult<_> {
            self.ensure_loaded()?;
            let parent = self.accounts_file
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            // Stream references into a unique sibling file: no complete JSON
            // buffer or duplicate account/password list is needed.
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            {
                let mut serializer = serde_json::Serializer::pretty(file.as_file_mut());
                let mut sequence = serializer.serialize_seq(None)?;
                for account in accounts {
                    sequence.serialize_element(account)?;
                }
                sequence.end()?;
            }
            file.as_file().sync_all()?;
            Ok(file)
        })();
        result.inspect_err(|error| {
            logging::record(
                Event::AccountsSaveFailed,
                Reason::from_error(error.as_ref()),
            );
        })
    }

    fn persist_accounts(&self, file: tempfile::NamedTempFile) -> ManagerResult<()> {
        file.persist(&self.accounts_file).map_err(|error| {
            logging::record(Event::AccountsSaveFailed, Reason::from_error(&error.error));
            Box::new(error.error) as Box<dyn Error + Send + Sync>
        })?;
        Ok(())
    }

    /// Store accepted accounts in one write; publish them in memory only after
    /// the file has been replaced. Skipped indices refer to the input vector.
    pub fn add_accounts(&mut self, accounts: Vec<Account>) -> ManagerResult<AddAccountsResult> {
        self.add_accounts_with(accounts, &NativePasswords(KEYRING_SERVICE))
    }

    fn add_accounts_with(
        &mut self,
        accounts: Vec<Account>,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<AddAccountsResult> {
        self.ensure_loaded()?;
        let mut keys: HashSet<_> = self
            .accounts
            .iter()
            .map(|account| {
                (
                    account.region.clone(),
                    account.account_id.to_ascii_lowercase(),
                )
            })
            .collect();
        let mut added = Vec::new();
        let mut skipped_indices = Vec::new();
        let mut previous_passwords = Vec::new();
        for (index, account) in accounts.into_iter().enumerate() {
            let key = (
                account.region.clone(),
                account.account_id.to_ascii_lowercase(),
            );
            if keys.contains(&key) {
                skipped_indices.push(index);
                continue;
            }
            if !account.password.is_empty() {
                let credential_key = format!("{}:{}", account.region, account.account_id);
                let stored = passwords.snapshot(&credential_key).and_then(|previous| {
                    passwords.set(&credential_key, &account.password)?;
                    Ok(previous)
                });
                match stored {
                    Ok(previous) => previous_passwords.push((credential_key, previous)),
                    Err(_) => {
                        skipped_indices.push(index);
                        continue;
                    }
                }
            }
            keys.insert(key);
            added.push(account);
        }
        if !added.is_empty() {
            let mut next = self.accounts.clone();
            next.extend(added.iter().cloned());
            sort_accounts(&mut next);
            let saved = self
                .prepare_accounts(&next)
                .and_then(|file| self.persist_accounts(file));
            if let Err(error) = saved {
                // Restore overwritten credentials as well as removing new ones.
                // Attempt every rollback even if an individual restore fails.
                let mut rollback_error = None;
                for (key, previous) in previous_passwords.into_iter().rev() {
                    let restored = passwords.restore(&key, previous);
                    if let Err(error) = restored {
                        rollback_error.get_or_insert(error);
                    }
                }
                return Err(rollback_error.unwrap_or(error));
            }
            self.accounts = next;
        }
        Ok(AddAccountsResult {
            added,
            skipped_indices,
        })
    }

    /// Change one account and publish the edited metadata and password only
    /// after both the account file and the credential store accepted them.
    ///
    /// `None` or an empty password leaves the stored credential untouched.
    /// The bool is `true` when the Riot ID changed, which resets the rank and
    /// invalidates refresh results admitted before the edit.
    pub fn update_account(
        &mut self,
        key: &AccountKey,
        changes: AccountChanges,
    ) -> ManagerResult<(Account, bool)> {
        self.update_account_with(key, changes, &NativePasswords(KEYRING_SERVICE))
    }

    fn update_account_with(
        &mut self,
        key: &AccountKey,
        changes: AccountChanges,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<(Account, bool)> {
        let index = self
            .accounts
            .iter()
            .position(|account| account.key() == *key)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "account not found")
            })?;
        let mut updated = self.accounts[index].clone();
        let renamed = updated.name != changes.name;
        updated.name = changes.name;
        updated.description = changes.description;
        updated.locale = changes.locale;
        updated.skip_profile_update = changes.skip_profile_update;
        if renamed {
            reset_rank(&mut updated);
        }
        let new_password = changes
            .password
            .as_deref()
            .filter(|password| !password.is_empty());
        if let Some(password) = new_password {
            updated.password = password.to_owned();
        }
        // Prepare the replacement first: a file failure must not drop the
        // credential that is still in use.
        let file = self.prepare_accounts(self.accounts.iter().enumerate().map(
            |(position, account)| if position == index { &updated } else { account },
        ))?;
        let credential_key = format!("{}:{}", key.region, key.account_id);
        let previous_password = match new_password {
            Some(password) => {
                let previous = passwords.snapshot(&credential_key)?;
                if let Err(error) = passwords.set(&credential_key, password) {
                    // Backends must not leave a partial replacement visible
                    // when they report a failed write.
                    passwords.restore(&credential_key, previous)?;
                    return Err(error);
                }
                Some(previous)
            }
            None => None,
        };
        if let Err(error) = self.persist_accounts(file) {
            if let Some(previous) = previous_password {
                passwords.restore(&credential_key, previous)?;
            }
            return Err(error);
        }
        if renamed {
            self.rank_jobs.remove(key);
        }
        self.accounts[index] = updated.clone();
        Ok((updated, renamed))
    }

    pub fn delete_account(&mut self, account_id: &str, region: &str) -> ManagerResult<()> {
        self.delete_account_with(account_id, region, &NativePasswords(KEYRING_SERVICE))
    }

    fn delete_account_with(
        &mut self,
        account_id: &str,
        region: &str,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<()> {
        let mut next = self.accounts.clone();
        next.retain(|account| !(account.account_id == account_id && account.region == region));
        if next.len() == self.accounts.len() {
            return Ok(());
        }
        // Prepare the replacement before deleting anything from the keyring.
        let file = self.prepare_accounts(&next)?;
        let key = format!("{region}:{account_id}");
        let previous = passwords.snapshot(&key)?;
        passwords.delete(&key)?;
        if let Err(error) = self.persist_accounts(file) {
            passwords.restore(&key, previous)?;
            return Err(error);
        }
        self.rank_jobs.remove(&AccountKey {
            account_id: account_id.to_owned(),
            region: region.to_owned(),
        });
        self.accounts = next;
        Ok(())
    }

    pub fn account(&self, key: &AccountKey) -> Option<&Account> {
        self.accounts.iter().find(|account| account.key() == *key)
    }

    pub fn account_mut(&mut self, key: &AccountKey) -> Option<&mut Account> {
        self.accounts
            .iter_mut()
            .find(|account| account.key() == *key)
    }

    /// Replace the credential and the cached password together. A failed
    /// credential write must leave login, copy and export using the old value.
    pub fn set_password(&mut self, key: &AccountKey, password: &str) -> ManagerResult<()> {
        self.set_password_with(key, password, &NativePasswords(KEYRING_SERVICE))
    }

    fn set_password_with(
        &mut self,
        key: &AccountKey,
        password: &str,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<()> {
        let account = self.account_mut(key).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "account not found")
        })?;
        let credential_key = format!("{}:{}", account.region, account.account_id);
        passwords.set(&credential_key, password)?;
        account.password = password.to_owned();
        Ok(())
    }

    /// Admit one account to a refresh. Every admission invalidates earlier
    /// tickets for the same account.
    pub fn begin_rank_refresh(&mut self, key: &AccountKey) -> Option<RankJob> {
        let account = self.account(key)?.clone();
        self.rank_generation = self.rank_generation.wrapping_add(1);
        self.rank_jobs.insert(key.clone(), self.rank_generation);
        Some(RankJob {
            account,
            generation: self.rank_generation,
        })
    }

    /// Apply a result only while its ticket is still the newest for the same
    /// key and Riot ID. Deleted, recreated, or renamed accounts reject older
    /// tickets.
    pub fn apply_rank_info(&mut self, job: &RankJob, info: &RankInfo) -> bool {
        let key = job.account.key();
        if self.rank_jobs.get(&key) != Some(&job.generation) {
            return false;
        }
        let Some(account) = self
            .account_mut(&key)
            .filter(|account| account.name == job.account.name)
        else {
            return false;
        };
        info.apply_to(account);
        true
    }

    /// Fetch all ranks with at most four concurrent provider calls.
    pub fn refresh_ranks(&mut self) -> ManagerResult<()> {
        logging::record(Event::RankRefreshStarted, Reason::None);
        let keys: Vec<AccountKey> = self.accounts.iter().map(Account::key).collect();
        let jobs: Vec<RankJob> = keys
            .iter()
            .filter_map(|key| self.begin_rank_refresh(key))
            .collect();
        let results = run_rank_jobs(self.rank_fetcher.clone(), jobs);
        for (job, info) in results {
            self.apply_rank_info(&job, &info);
        }
        sort_accounts(&mut self.accounts);
        self.save_accounts()?;
        logging::record(Event::RankRefreshCompleted, Reason::None);
        Ok(())
    }

    /// Serialize every field, including passwords, for an explicit export.
    pub fn export_accounts(&mut self) -> ManagerResult<String> {
        let mut export = Vec::with_capacity(self.accounts.len());
        for account in &mut self.accounts {
            if account.password.is_empty() {
                account.password = credentials::get_password(
                    KEYRING_SERVICE,
                    &format!("{}:{}", account.region, account.account_id),
                )
                .ok()
                .flatten()
                .unwrap_or_default();
            }
            export.push(ExportAccount::from(&*account));
        }
        Ok(serde_json::to_string_pretty(&export).inspect_err(|error| {
            logging::record(Event::ExportFailed, Reason::from_error(error));
        })?)
    }

    /// Import a JSON array. Returns `(added, skipped)` like the original app.
    pub fn import_accounts(&mut self, json: &str) -> ManagerResult<(usize, usize)> {
        self.import_accounts_with(json, &NativePasswords(KEYRING_SERVICE))
    }

    fn import_accounts_with(
        &mut self,
        json: &str,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<(usize, usize)> {
        let data: Vec<ImportAccount> = serde_json::from_str(json).inspect_err(|error| {
            logging::record(Event::ImportFailed, Reason::from_error(error));
        })?;
        let mut accounts = Vec::with_capacity(data.len());
        let mut skipped = 0;
        for (index, item) in data.into_iter().enumerate() {
            let account_id = item.account_id.trim().to_owned();
            if account_id.is_empty() {
                skipped += 1;
                continue;
            }
            let Some(region) = normalize_import_region(&item.region) else {
                return Err(import_error(index, "unsupported region"));
            };
            let locale = match normalized_import_locale(item.locale.as_deref()) {
                Ok(locale) => locale,
                Err(()) => return Err(import_error(index, "invalid locale")),
            };
            let account = Account {
                account_id: account_id.clone(),
                name: item.name,
                riot_id_not_found: item.riot_id_not_found,
                region: region.clone(),
                region_display: if item.region_display.is_empty() {
                    region_display(&region)
                } else {
                    item.region_display
                },
                password: item.password,
                description: item.description,
                tier: if item.tier.is_empty() {
                    "Unranked".to_owned()
                } else {
                    item.tier
                },
                division: item.division,
                lp: item.lp,
                level: item.level,
                reached_last_season: if item.reached_last_season.is_empty() {
                    "N/A".to_owned()
                } else {
                    item.reached_last_season
                },
                finished_last_season: if item.finished_last_season.is_empty() {
                    "N/A".to_owned()
                } else {
                    item.finished_last_season
                },
                tft: item.tft,
                locale,
                skip_profile_update: item.skip_profile_update,
                rank_provenance: item.rank_provenance,
                tft_provenance: item.tft_provenance,
                // Re-detected at the next sign-in.
                ban: None,
            };
            accounts.push(account);
        }
        let result = self.add_accounts_with(accounts, passwords)?;
        Ok((result.added.len(), skipped + result.skipped_indices.len()))
    }
}

/// Map an exported region (internal code in any case, or a known display
/// label) onto the internal code used everywhere else.
fn normalize_import_region(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    REGION_MAP
        .iter()
        .find(|(label, code)| {
            label.eq_ignore_ascii_case(value) || code.eq_ignore_ascii_case(value)
        })
        .map(|(_, code)| (*code).to_owned())
}

/// Empty values keep Riot's own choice; anything not a known alias is rejected
/// rather than stripped or guessed into a writable value.
fn normalized_import_locale(value: Option<&str>) -> Result<Option<String>, ()> {
    if value.is_some_and(|value| value.chars().any(char::is_control)) {
        return Err(());
    }
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => locales::normalize(value)
            .map(|locale| Some(locale.to_owned()))
            .ok_or(()),
    }
}

/// Point at the failing import entry without echoing its values.
fn import_error(index: usize, problem: &str) -> Box<dyn Error + Send + Sync> {
    let error = std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("import item {index}: {problem}"),
    );
    logging::record(Event::ImportFailed, Reason::from_error(&error));
    Box::new(error)
}

/// A renamed account no longer matches the Riot ID its ranks were fetched for.
fn reset_rank(account: &mut Account) {
    account.riot_id_not_found = false;
    RankInfo::unranked().apply_to(account);
}

fn run_rank_jobs(
    provider: Arc<dyn RankProvider>,
    jobs: Vec<RankJob>,
) -> Vec<(RankJob, RankInfo)> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let (sender, receiver) = mpsc::channel();
    let worker_count = queue.lock().map(|queue| queue.len().min(4)).unwrap_or(1);
    let mut workers = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let sender = sender.clone();
        let provider = Arc::clone(&provider);
        workers.push(thread::spawn(move || loop {
            let job = queue.lock().ok().and_then(|mut queue| queue.pop_front());
            let Some(job) = job else { break };
            // The ticket owns the only account copy; the provider borrows it.
            let result = {
                let account = &job.account;
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    provider.fetch_rank(account)
                }))
                .unwrap_or_else(|_| {
                    logging::record(Event::RankWorkerPanicked, Reason::None);
                    RankInfo::error()
                })
            };
            if sender.send((job, result)).is_err() {
                break;
            }
        }));
    }
    drop(sender);
    let results: Vec<(RankJob, RankInfo)> = receiver.into_iter().collect();
    for worker in workers {
        let _ = worker.join();
    }
    results
}

#[derive(Clone, Debug, Serialize)]
struct ExportAccount {
    account_id: String,
    name: String,
    riot_id_not_found: bool,
    region: String,
    region_display: String,
    password: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
    skip_profile_update: bool,
    tier: String,
    division: String,
    lp: String,
    level: String,
    reached_last_season: String,
    finished_last_season: String,
    tft: TftRank,
    rank_provenance: Option<RankProvenance>,
    tft_provenance: Option<RankProvenance>,
}

impl From<&Account> for ExportAccount {
    fn from(account: &Account) -> Self {
        Self {
            account_id: account.account_id.clone(),
            name: account.name.clone(),
            riot_id_not_found: account.riot_id_not_found,
            region: account.region.clone(),
            region_display: account.region_display.clone(),
            password: account.password.clone(),
            description: account.description.clone(),
            locale: account.locale.clone(),
            skip_profile_update: account.skip_profile_update,
            tier: account.tier.clone(),
            division: account.division.clone(),
            lp: account.lp.clone(),
            level: account.level.clone(),
            reached_last_season: account.reached_last_season.clone(),
            finished_last_season: account.finished_last_season.clone(),
            tft: account.tft.clone(),
            rank_provenance: account.rank_provenance.clone(),
            tft_provenance: account.tft_provenance.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ImportAccount {
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    riot_id_not_found: bool,
    #[serde(default)]
    region: String,
    #[serde(default)]
    region_display: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    skip_profile_update: bool,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    division: String,
    #[serde(default)]
    lp: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    reached_last_season: String,
    #[serde(default)]
    finished_last_season: String,
    #[serde(default)]
    tft: TftRank,
    #[serde(default)]
    rank_provenance: Option<RankProvenance>,
    #[serde(default)]
    tft_provenance: Option<RankProvenance>,
}
