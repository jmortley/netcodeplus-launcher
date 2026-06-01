//! UT4 master-server login.
//!
//! Lets the launcher own UT4 account login (and bypass UT4UU's in-game login
//! window) via the master server's Epic-style OAuth:
//!
//! 1. `grant_type=password` once to bootstrap a session.
//! 2. The refresh token is stored in the **OS credential store** (the only
//!    secret; never `state.json`, never the OneDrive-synced Documents tree).
//! 3. Each launch: `grant_type=refresh_token` for a fresh access token (and a
//!    rotated refresh token), then `GET oauth/exchange` for a one-shot exchange
//!    code, which is handed to the game via the stock Epic auth-handoff args
//!    `-AUTH_LOGIN=<user> -AUTH_PASSWORD=<code> -AUTH_TYPE=exchangecode`.
//!
//! Endpoints/clients/lifetimes verified against the timiimit/UT4MasterServer
//! source; the launch-arg convention confirmed by timiimit directly.

use serde::Serialize;

use crate::commands::state_path;

/// OAuth token endpoint on timiimit's master server (the same host the launcher
/// writes into Engine.ini's `[OnlineSubsystemMcp.*]` sections).
const TOKEN_URL: &str = "https://master-ut4.timiimit.com/account/api/oauth/token";
/// OAuth exchange-code endpoint (mints the one-shot code the game redeems).
const EXCHANGE_URL: &str = "https://master-ut4.timiimit.com/account/api/oauth/exchange";

/// HTTP Basic header for the stock UT4 "Launcher" OAuth client — the public
/// `id:secret` from `ClientIdentification.cs` (`34a02cf8…:daafbccc…`), which is
/// the client permitted to perform `password` grants. These are well-known UT4
/// constants shipped in the game, not a private secret.
const LAUNCHER_BASIC: &str =
    "Basic MzRhMDJjZjhmNDQxNGUyOWIxNTkyMTg3NmRhMzZmOWE6ZGFhZmJjY2M3Mzc3NDUwMzlkZmZlNTNkOTRmYzc2Y2Y=";

/// Credential-store coordinates for the one stored secret (the refresh token).
/// The launcher tracks a single UT4 account, so a fixed key is sufficient.
const KEYRING_SERVICE: &str = "netcodeplus-launcher";
const KEYRING_USER: &str = "ut4-refresh-token";

/// Cap on an OAuth response body. Token/exchange responses are tiny.
const RESP_MAX: u64 = 64 * 1024;

/// Error string the frontend matches on: the refresh token is gone or rejected
/// (expired/revoked), so the user must sign in with their password again.
const RELOGIN: &str = "RELOGIN_REQUIRED";

/// Subset of the OAuth token response we consume. Unknown fields are ignored.
#[derive(serde::Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    /// Canonical account id (the master server's `account.ID`). Stable across
    /// email-vs-username logins; used as the game's `-epicuserid` launch arg so
    /// it boots straight into this account instead of the account picker.
    #[serde(default)]
    account_id: String,
}

/// The OAuth exchange response — carries the one-shot code in `code`.
#[derive(serde::Deserialize)]
struct ExchangeResponse {
    #[serde(default)]
    code: String,
}

/// UT4 account status surfaced to the webview.
#[derive(Serialize)]
pub struct Ut4Auth {
    /// Whether a refresh token is on file (i.e. the user is signed in).
    pub logged_in: bool,
    /// Account username (the `-AUTH_LOGIN` value), if signed in.
    pub username: Option<String>,
    /// Account display name for the UI, if signed in.
    pub display_name: Option<String>,
    /// Set by [`ut4_login`] only when this sign-in replaced a *different* account
    /// that was already signed in: the display name of that previous account, so
    /// the UI can warn "now signed in as X (was Y)". `None` for a first sign-in, a
    /// same-account re-login, or [`ut4_auth_status`].
    #[serde(default)]
    pub switched_from: Option<String>,
}

/// What a launch needs to authenticate the game without the in-game window.
#[derive(Serialize)]
pub struct Ut4LaunchAuth {
    /// `-AUTH_LOGIN` value.
    pub username: String,
    /// One-shot `-AUTH_PASSWORD` value (used with `-AUTH_TYPE=exchangecode`).
    pub exchange_code: String,
    /// Canonical account id for the game's `-epicuserid` arg, so it boots into
    /// this account rather than showing the account picker. Empty when the
    /// stored session predates account-id capture (the user must re-login once);
    /// the frontend omits the arg when this is empty.
    pub account_id: String,
}

fn keyring_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| e.to_string())
}

fn store_refresh(token: &str) -> Result<(), String> {
    keyring_entry()?
        .set_password(token)
        .map_err(|e| e.to_string())
}

fn load_refresh() -> Result<Option<String>, String> {
    match keyring_entry()?.get_password() {
        Ok(t) => Ok(Some(t)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn clear_refresh() {
    if let Ok(entry) = keyring_entry() {
        // Ignore "no entry" / backend errors — logout is best-effort.
        let _ = entry.delete_credential();
    }
}

/// POST a form to the token endpoint with the Launcher client's Basic auth.
async fn post_token(form: &[(&str, &str)]) -> Result<String, ncp_net::NetError> {
    let client = ncp_net::Client::new()?;
    ncp_net::post_form(
        &client,
        TOKEN_URL,
        &[("authorization", LAUNCHER_BASIC)],
        form,
        RESP_MAX,
    )
    .await
}

/// Exchange an access token for a one-shot exchange code.
async fn mint_exchange(access_token: &str) -> Result<String, String> {
    let client = ncp_net::Client::new().map_err(|e| e.to_string())?;
    let bearer = format!("Bearer {access_token}");
    let body = ncp_net::fetch_text_with_headers(
        &client,
        EXCHANGE_URL,
        &[("authorization", &bearer)],
        RESP_MAX,
    )
    .await
    .map_err(|e| e.to_string())?;
    let resp: ExchangeResponse =
        serde_json::from_str(&body).map_err(|e| format!("bad exchange response: {e}"))?;
    if resp.code.is_empty() {
        return Err("master server returned no exchange code".into());
    }
    Ok(resp.code)
}

/// Map a token-endpoint error to a friendly login-failure message, treating any
/// 4xx as bad credentials.
fn login_error(e: ncp_net::NetError) -> String {
    match e {
        ncp_net::NetError::HttpStatus { status, .. } if (400..500).contains(&status) => {
            "Login failed — check your username and password.".to_string()
        }
        other => other.to_string(),
    }
}

/// Sign in with a UT4 username + password (`password` grant). Stores only the
/// refresh token in the OS credential store; persists the (non-secret) username
/// and display name in `state.json`. The password is never persisted.
#[tauri::command]
pub async fn ut4_login(
    app: tauri::AppHandle,
    username: String,
    password: String,
) -> Result<Ut4Auth, String> {
    let username = username.trim().to_string();
    if username.is_empty() || password.is_empty() {
        return Err("Enter your UT4 username and password.".into());
    }

    // Was a session already active? Captured before `store_refresh` overwrites
    // the keyring, so we can warn below about replacing a *different* account.
    let was_logged_in = load_refresh().unwrap_or(None).is_some();

    let body = post_token(&[
        ("grant_type", "password"),
        ("username", &username),
        ("password", &password),
    ])
    .await
    .map_err(login_error)?;

    let tok: TokenResponse =
        serde_json::from_str(&body).map_err(|e| format!("bad token response: {e}"))?;
    if tok.refresh_token.is_empty() {
        return Err("master server returned no refresh token".into());
    }
    store_refresh(&tok.refresh_token)?;

    let display = if tok.display_name.is_empty() {
        username.clone()
    } else {
        tok.display_name.clone()
    };

    let path = state_path(&app)?;
    let mut st = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    // The canonical account id for the `-epicuserid` launch arg. Empty if the
    // server omitted it; the launch path simply skips the arg then.
    let new_account_id = if tok.account_id.is_empty() {
        None
    } else {
        Some(tok.account_id.clone())
    };
    // A "switch" = a session was already active and this sign-in resolved to a
    // *different* account (compared by canonical id). The display name of the
    // replaced account drives the UI's "now signed in as X (was Y)" warning, so
    // an accidental login to the wrong account is caught before a launch writes
    // it into UT4's saved-account list. Needs both ids; an empty new id can't be
    // proven different, so it never warns. Read before we overwrite the fields.
    let switched_from = if was_logged_in
        && st.ut4_account_id.is_some()
        && new_account_id.is_some()
        && st.ut4_account_id != new_account_id
    {
        st.ut4_display_name
            .clone()
            .or_else(|| st.ut4_username.clone())
    } else {
        None
    };

    st.ut4_username = Some(username.clone());
    st.ut4_display_name = Some(display.clone());
    st.ut4_account_id = new_account_id;
    ncp_host::state::write(&path, &st).map_err(|e| e.to_string())?;

    Ok(Ut4Auth {
        logged_in: true,
        username: Some(username),
        display_name: Some(display),
        switched_from,
    })
}

/// Current UT4 login status (does a refresh token exist + the cached account
/// info). A credential-store error is treated as "signed out" so the UI degrades
/// gracefully rather than erroring on load.
#[tauri::command]
pub fn ut4_auth_status(app: tauri::AppHandle) -> Result<Ut4Auth, String> {
    let logged_in = load_refresh().unwrap_or(None).is_some();
    let st = ncp_host::state::read(&state_path(&app)?)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(Ut4Auth {
        logged_in,
        username: if logged_in { st.ut4_username } else { None },
        display_name: if logged_in { st.ut4_display_name } else { None },
        switched_from: None,
    })
}

/// Sign out: delete the stored refresh token and clear the cached account info.
#[tauri::command]
pub fn ut4_logout(app: tauri::AppHandle) -> Result<(), String> {
    clear_refresh();
    let path = state_path(&app)?;
    let mut st = ncp_host::state::read(&path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    st.ut4_username = None;
    st.ut4_display_name = None;
    st.ut4_account_id = None;
    ncp_host::state::write(&path, &st).map_err(|e| e.to_string())
}

/// Produce fresh launch credentials: refresh the session (rotating + re-storing
/// the refresh token), then mint a one-shot exchange code. Returns the username
/// + code for the `-AUTH_*` launch args.
///
/// Returns the `RELOGIN_REQUIRED` error when there is no stored token or the
/// refresh is rejected (expired/revoked >~20 days) — the frontend then prompts
/// for the password and calls [`ut4_login`] before retrying the launch.
#[tauri::command]
pub async fn ut4_prepare_launch(app: tauri::AppHandle) -> Result<Ut4LaunchAuth, String> {
    let refresh = load_refresh()?.ok_or_else(|| RELOGIN.to_string())?;

    let body = post_token(&[("grant_type", "refresh_token"), ("refresh_token", &refresh)])
        .await
        .map_err(|e| match e {
            ncp_net::NetError::HttpStatus { status, .. } if (400..500).contains(&status) => {
                RELOGIN.to_string()
            }
            other => other.to_string(),
        })?;

    let tok: TokenResponse =
        serde_json::from_str(&body).map_err(|e| format!("bad token response: {e}"))?;
    if tok.access_token.is_empty() {
        return Err(RELOGIN.to_string());
    }
    // Rotate the stored refresh token (best-effort — a stale one just forces a
    // re-login next time).
    if !tok.refresh_token.is_empty() {
        let _ = store_refresh(&tok.refresh_token);
    }

    let exchange_code = mint_exchange(&tok.access_token).await?;

    let st = ncp_host::state::read(&state_path(&app)?)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let username = st.ut4_username.ok_or_else(|| RELOGIN.to_string())?;

    Ok(Ut4LaunchAuth {
        username,
        exchange_code,
        // Empty string when not captured (pre-account-id-capture session);
        // the frontend then omits `-epicuserid` rather than send it empty.
        account_id: st.ut4_account_id.unwrap_or_default(),
    })
}
