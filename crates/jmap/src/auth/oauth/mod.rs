use std::{collections::HashMap, sync::atomic::AtomicU32};

use hyper::{header::CONTENT_TYPE, StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::{
    http::{fetch_body, ToHttpResponse},
    HtmlResponse, HttpRequest, HttpResponse,
};

pub mod device_auth;
pub mod token;
pub mod user_code;

const OAUTH_HTML_HEADER: &str = include_str!("../../../../../resources/oauth/header.htx");
const OAUTH_HTML_FOOTER: &str = include_str!("../../../../../resources/oauth/footer.htx");
const OAUTH_HTML_LOGIN_HEADER_CLIENT: &str =
    include_str!("../../../../../resources/oauth/login_hdr_client.htx");
const OAUTH_HTML_LOGIN_HEADER_DEVICE: &str =
    include_str!("../../../../../resources/oauth/login_hdr_device.htx");
const OAUTH_HTML_LOGIN_HEADER_FAILED: &str =
    include_str!("../../../../../resources/oauth/login_hdr_failed.htx");
const OAUTH_HTML_LOGIN_FORM: &str = include_str!("../../../../../resources/oauth/login.htx");
const OAUTH_HTML_LOGIN_CODE: &str = include_str!("../../../../../resources/oauth/login_code.htx");
const OAUTH_HTML_LOGIN_CODE_HIDDEN: &str =
    include_str!("../../../../../resources/oauth/login_code_hidden.htx");
const OAUTH_HTML_LOGIN_SUCCESS: &str =
    include_str!("../../../../../resources/oauth/login_success.htx");
const OAUTH_HTML_ERROR: &str = include_str!("../../../../../resources/oauth/error.htx");

const STATUS_AUTHORIZED: u32 = 0;
const STATUS_TOKEN_ISSUED: u32 = 1;
const STATUS_PENDING: u32 = 2;

const DEVICE_CODE_LEN: usize = 40;
const USER_CODE_LEN: usize = 8;
const RANDOM_CODE_LEN: usize = 32;
const CLIENT_ID_MAX_LEN: usize = 20;

const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // No 0, O, I, 1

pub struct OAuth {
    pub key: String,
    pub expiry_user_code: u64,
    pub expiry_auth_code: u64,
    pub expiry_token: u64,
    pub expiry_refresh_token: u64,
    pub expiry_refresh_token_renew: u64,
    pub max_auth_attempts: u32,
    pub metadata: String,
}

pub struct OAuthCode {
    pub status: AtomicU32,
    pub account_id: AtomicU32,
    pub client_id: String,
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceAuthGet {
    code: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceAuthPost {
    code: Option<String>,
    email: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceAuthRequest {
    client_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CodeAuthRequest {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    scope: Option<String>,
    state: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CodeAuthForm {
    code: String,
    email: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: Option<String>,
    pub device_code: Option<String>,
    pub client_id: Option<String>,
    pub refresh_token: Option<String>,
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TokenResponse {
    Granted {
        access_token: String,
        token_type: String,
        expires_in: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        refresh_token: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    Error {
        error: ErrorType,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorType {
    #[serde(rename = "invalid_grant")]
    InvalidGrant,
    #[serde(rename = "invalid_client")]
    InvalidClient,
    #[serde(rename = "invalid_scope")]
    InvalidScope,
    #[serde(rename = "invalid_request")]
    InvalidRequest,
    #[serde(rename = "unauthorized_client")]
    UnauthorizedClient,
    #[serde(rename = "unsupported_grant_type")]
    UnsupportedGrantType,
    #[serde(rename = "authorization_pending")]
    AuthorizationPending,
    #[serde(rename = "slow_down")]
    SlowDown,
    #[serde(rename = "access_denied")]
    AccessDenied,
    #[serde(rename = "expired_token")]
    ExpiredToken,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OAuthMetadata {
    pub issuer: String,
    pub token_endpoint: String,
    pub grant_types_supported: Vec<String>,
    pub device_authorization_endpoint: String,
    pub response_types_supported: Vec<String>,
    pub scopes_supported: Vec<String>,
    pub authorization_endpoint: String,
}

impl OAuthMetadata {
    pub fn new(base_url: &str) -> Self {
        OAuthMetadata {
            issuer: base_url.to_string(),
            authorization_endpoint: format!("{}/auth/code", base_url),
            token_endpoint: format!("{}/auth/token", base_url),
            grant_types_supported: vec![
                "authorization_code".to_string(),
                "implicit".to_string(),
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ],
            device_authorization_endpoint: format!("{}/auth/device", base_url),
            response_types_supported: vec!["code".to_string(), "code token".to_string()],
            scopes_supported: vec!["offline_access".to_string()],
        }
    }
}

impl TokenResponse {
    pub fn error(error: ErrorType) -> Self {
        TokenResponse::Error { error }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, TokenResponse::Error { .. })
    }
}

pub async fn parse_form_data(
    req: &mut HttpRequest,
) -> Result<HashMap<String, String>, HttpResponse> {
    match (
        req.headers()
            .get(CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .and_then(|val| val.parse::<mime::Mime>().ok()),
        fetch_body(req, 2048).await,
    ) {
        (Some(content_type), Ok(body)) => {
            let mut fields = HashMap::new();
            if let Some(boundary) = content_type.get_param(mime::BOUNDARY) {
                for mut field in form_data::FormData::new(&body[..], boundary.as_str()).flatten() {
                    let value = String::from_utf8(field.bytes().unwrap_or_default().to_vec())
                        .unwrap_or_default();
                    fields.insert(field.name, value);
                }
            } else {
                for (key, value) in form_urlencoded::parse(&body) {
                    fields.insert(key.into_owned(), value.into_owned());
                }
            }
            Ok(fields)
        }
        _ => Err(HtmlResponse::with_status(
            StatusCode::BAD_REQUEST,
            "Invalid post request".to_string(),
        )
        .into_http_response()),
    }
}
