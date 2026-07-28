use serde::{Deserialize, Serialize};

use super::TypedId;

pub const SCHEMA_ID: &str = "accounts";
const ACCOUNT_ID_PREFIX: &str = "account";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountsFile {
    #[serde(default)]
    pub accounts: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl AccountsFile {
    pub fn known_accounts(&self) -> Vec<KnownAccount> {
        self.accounts
            .iter()
            .filter_map(
                |account| match account.get("type").and_then(|value| value.as_str()) {
                    Some("offline") => serde_json::from_value(account.clone())
                        .ok()
                        .map(KnownAccount::Offline),
                    Some("microsoft") => serde_json::from_value(account.clone())
                        .ok()
                        .map(KnownAccount::Microsoft),
                    Some("authlibInjector") => serde_json::from_value(account.clone())
                        .ok()
                        .map(KnownAccount::AuthlibInjector),
                    _ => None,
                },
            )
            .collect()
    }

    pub fn offline_accounts(&self) -> Vec<OfflineAccountEntry> {
        self.accounts
            .iter()
            .filter(|a| a.get("type").and_then(|t| t.as_str()) == Some("offline"))
            .filter_map(|a| serde_json::from_value(a.clone()).ok())
            .collect()
    }

    pub fn upsert_offline_account(&mut self, entry: &OfflineAccountEntry) {
        let value = serde_json::to_value(entry).expect("OfflineAccountEntry must always serialize");
        let account_id = value.get("accountID").cloned();
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|a| a.get("accountID") == account_id.as_ref())
        {
            *existing = value;
        } else {
            self.accounts.push(value);
        }
    }

    pub fn microsoft_accounts(&self) -> Vec<MicrosoftAccountEntry> {
        self.accounts
            .iter()
            .filter(|a| a.get("type").and_then(|t| t.as_str()) == Some("microsoft"))
            .filter_map(|a| serde_json::from_value(a.clone()).ok())
            .collect()
    }

    pub fn upsert_microsoft_account(&mut self, entry: &MicrosoftAccountEntry) {
        let value =
            serde_json::to_value(entry).expect("MicrosoftAccountEntry must always serialize");
        let account_id = value.get("accountID").cloned();
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|account| account.get("accountID") == account_id.as_ref())
        {
            *existing = value;
        } else {
            self.accounts.push(value);
        }
    }

    pub fn authlib_injector_accounts(&self) -> Vec<AuthlibInjectorAccountEntry> {
        self.accounts
            .iter()
            .filter(|account| {
                account.get("type").and_then(|value| value.as_str()) == Some("authlibInjector")
            })
            .filter_map(|account| serde_json::from_value(account.clone()).ok())
            .collect()
    }

    pub fn upsert_authlib_injector_account(&mut self, entry: &AuthlibInjectorAccountEntry) {
        let value =
            serde_json::to_value(entry).expect("AuthlibInjectorAccountEntry must always serialize");
        let account_id = value.get("accountID").cloned();
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|account| account.get("accountID") == account_id.as_ref())
        {
            *existing = value;
        } else {
            self.accounts.push(value);
        }
    }

    pub fn remove_account(&mut self, account_id: &str) -> bool {
        let before = self.accounts.len();
        self.accounts
            .retain(|a| a.get("accountID").and_then(|v| v.as_str()) != Some(account_id));
        before != self.accounts.len()
    }
}

#[derive(Debug, Clone)]
pub enum KnownAccount {
    Offline(OfflineAccountEntry),
    Microsoft(MicrosoftAccountEntry),
    AuthlibInjector(AuthlibInjectorAccountEntry),
}

impl KnownAccount {
    pub fn account_id(&self) -> &str {
        match self {
            Self::Offline(account) => &account.account_id,
            Self::Microsoft(account) => &account.account_id,
            Self::AuthlibInjector(account) => &account.account_id,
        }
    }

    pub fn profile_name(&self) -> &str {
        match self {
            Self::Offline(account) => &account.profile_name,
            Self::Microsoft(account) => &account.profile_name,
            Self::AuthlibInjector(account) => &account.profile_name,
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            Self::Offline(_) => "offline",
            Self::Microsoft(_) => "microsoft",
            Self::AuthlibInjector(_) => "authlibInjector",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthlibInjectorAccountEntry {
    #[serde(rename = "accountID")]
    pub account_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "profileID")]
    pub profile_id: String,
    #[serde(rename = "profileName")]
    pub profile_name: String,
    #[serde(rename = "loginName")]
    pub login_name: String,
    #[serde(rename = "serverBaseURL")]
    pub server_base_url: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl AuthlibInjectorAccountEntry {
    pub fn new(
        login_name: impl Into<String>,
        server_base_url: impl Into<String>,
        profile: &crate::auth::authlib_injector::GameProfile,
    ) -> Self {
        Self {
            account_id: TypedId::generate(ACCOUNT_ID_PREFIX).to_string(),
            kind: "authlibInjector".to_string(),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            login_name: login_name.into(),
            server_base_url: server_base_url.into(),
            extra: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthlibInjectorAccountTokensFile {
    #[serde(default)]
    pub accounts:
        std::collections::BTreeMap<String, crate::auth::authlib_injector::AuthlibInjectorSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicrosoftAccountEntry {
    #[serde(rename = "accountID")]
    pub account_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "profileID")]
    pub profile_id: String,
    #[serde(rename = "profileName")]
    pub profile_name: String,
}

impl MicrosoftAccountEntry {
    pub fn from_session(session: &crate::auth::microsoft::MicrosoftSession) -> Self {
        Self {
            account_id: TypedId::generate(ACCOUNT_ID_PREFIX).to_string(),
            kind: "microsoft".to_string(),
            profile_id: session.profile_id.clone(),
            profile_name: session.profile_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MicrosoftAccountTokensFile {
    #[serde(default)]
    pub accounts: std::collections::BTreeMap<String, crate::auth::microsoft::MicrosoftSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineAccountEntry {
    #[serde(rename = "accountID")]
    pub account_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// 对应 Java `OfflineAccountFactory.fromStorage`：这个字段可以缺失，缺失时
    /// 读取方要用 [`Self::resolved_profile_id`] 从用户名派生，不能直接 unwrap。
    #[serde(rename = "profileID", default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(rename = "profileName")]
    pub profile_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<Skin>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl OfflineAccountEntry {
    pub fn new(username: impl Into<String>) -> OfflineAccountEntry {
        let username = username.into();
        let profile_id = crate::auth::offline_player_uuid(&username).to_string();
        OfflineAccountEntry {
            account_id: TypedId::generate(ACCOUNT_ID_PREFIX).to_string(),
            kind: "offline".to_string(),
            profile_id: Some(profile_id),
            profile_name: username,
            skin: None,
            extra: Default::default(),
        }
    }

    pub fn resolved_profile_id(&self) -> uuid::Uuid {
        match &self.profile_id {
            Some(s) => uuid::Uuid::parse_str(s)
                .unwrap_or_else(|_| crate::auth::offline_player_uuid(&self.profile_name)),
            None => crate::auth::offline_player_uuid(&self.profile_name),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skin {
    #[serde(rename = "type", default = "default_skin_type")]
    pub kind: String,
    #[serde(rename = "cslApi", default, skip_serializing_if = "Option::is_none")]
    pub csl_api: Option<String>,
    #[serde(rename = "textureModel", default = "default_texture_model")]
    pub texture_model: String,
    #[serde(
        rename = "localSkinPath",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub local_skin_path: Option<String>,
    #[serde(
        rename = "localCapePath",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub local_cape_path: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_skin_type() -> String {
    "default".to_string()
}
fn default_texture_model() -> String {
    "wide".to_string()
}

impl Skin {
    pub fn local_file(path: impl Into<String>) -> Skin {
        Skin {
            kind: "local_file".to_string(),
            csl_api: None,
            texture_model: default_texture_model(),
            local_skin_path: Some(path.into()),
            local_cape_path: None,
            extra: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_offline_account_derives_profile_id_matching_the_known_algorithm() {
        let entry = OfflineAccountEntry::new("Steve");
        assert_eq!(
            entry.profile_id.as_deref(),
            Some(
                crate::auth::offline_player_uuid("Steve")
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(
            entry.resolved_profile_id(),
            crate::auth::offline_player_uuid("Steve")
        );
    }

    #[test]
    fn missing_profile_id_is_derived_from_username_on_read() {
        let mut entry = OfflineAccountEntry::new("Alex");
        entry.profile_id = None; // 模拟一份没有写 profileID 的旧配置
        assert_eq!(
            entry.resolved_profile_id(),
            crate::auth::offline_player_uuid("Alex")
        );
    }

    #[test]
    fn non_offline_accounts_survive_round_trip_untouched() {
        let mut file = AccountsFile::default();
        file.accounts.push(serde_json::json!({
            "accountID": "account:11111111-1111-1111-1111-111111111111",
            "type": "microsoft",
            "someFieldWeDoNotUnderstand": {"nested": true}
        }));

        assert!(
            file.offline_accounts().is_empty(),
            "microsoft account must not be misparsed as offline"
        );

        let offline = OfflineAccountEntry::new("Steve");
        file.upsert_offline_account(&offline);
        assert_eq!(
            file.accounts.len(),
            2,
            "adding an offline account must not disturb the untouched microsoft entry"
        );

        let microsoft_entry = &file.accounts[0];
        assert_eq!(
            microsoft_entry["someFieldWeDoNotUnderstand"]["nested"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn upsert_replaces_existing_entry_by_account_id() {
        let mut file = AccountsFile::default();
        let mut entry = OfflineAccountEntry::new("Steve");
        file.upsert_offline_account(&entry);
        assert_eq!(file.accounts.len(), 1);

        entry.skin = Some(Skin::local_file("C:/skins/steve.png"));
        file.upsert_offline_account(&entry);
        assert_eq!(
            file.accounts.len(),
            1,
            "same accountID must overwrite, not append"
        );
        assert_eq!(
            file.offline_accounts()[0]
                .skin
                .as_ref()
                .unwrap()
                .local_skin_path
                .as_deref(),
            Some("C:/skins/steve.png")
        );
    }

    #[test]
    fn remove_account_drops_only_the_matching_entry() {
        let mut file = AccountsFile::default();
        let a = OfflineAccountEntry::new("Steve");
        let b = OfflineAccountEntry::new("Alex");
        file.upsert_offline_account(&a);
        file.upsert_offline_account(&b);

        assert!(file.remove_account(&a.account_id));
        assert_eq!(file.accounts.len(), 1);
        assert_eq!(file.offline_accounts()[0].profile_name, "Alex");
        assert!(!file.remove_account("account:00000000-0000-0000-0000-000000000000"));
    }
}
