//! Checking a stored login with the cheapest real request the vendor offers.
//!
//! The request is described by the framework's catalog entry (`[framework.verify]`):
//! a GET to a read-only endpoint with the secret as a bearer token or a named
//! header. The outcome is `ok` (2xx), `invalid` (a status the catalog lists as
//! "rejected"), or `unknown` (anything else: the network is down, the vendor is
//! rate limiting, the endpoint does not accept this kind of login). The secret
//! is only ever placed in a request header; it is never part of a message.

use std::time::Duration;

use ta_credentials::agent_login::{Framework, Secret, State, VerifyAuth, VerifyMethod};

/// Seconds the verify request may take.
pub const VERIFY_TIMEOUT_ENV: &str = "TA_AGENT_LOGIN_VERIFY_TIMEOUT_SECS";
const DEFAULT_VERIFY_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyResult {
    pub state: State,
    pub detail: String,
}

fn timeout() -> Duration {
    let secs = std::env::var(VERIFY_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_VERIFY_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("::1")
    )
}

/// Verify `secret` for `fw` with a real request.
pub fn verify(fw: &Framework, secret: &Secret) -> VerifyResult {
    let unknown = |detail: String| VerifyResult {
        state: State::Unknown,
        detail,
    };
    if fw.verify.method == VerifyMethod::None {
        return unknown(format!(
            "{} has no inexpensive check, so the login is kept as given and was not verified.",
            fw.label
        ));
    }
    let url = match reqwest::Url::parse(&fw.verify.url) {
        Ok(u) => u,
        Err(e) => {
            return unknown(format!(
                "the verify address '{}' for {} is not a valid URL ({e}); fix `verify.url` in \
                 agent-frameworks.toml.",
                fw.verify.url, fw.label
            ))
        }
    };
    let host = url.host_str().unwrap_or("the vendor").to_string();
    let secs = timeout().as_secs();

    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in &fw.verify.headers {
        match (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            (Ok(n), Ok(v)) => {
                headers.insert(n, v);
            }
            _ => {
                return unknown(format!(
                    "the header '{name}' in the verify settings of {} is not valid; fix \
                     `verify.headers` in agent-frameworks.toml.",
                    fw.label
                ))
            }
        }
    }
    let (auth_name, auth_value) = match fw.verify.auth {
        VerifyAuth::Bearer => (
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", secret.expose()),
        ),
        VerifyAuth::Header => {
            match reqwest::header::HeaderName::from_bytes(fw.verify.header.as_bytes()) {
                Ok(n) => (n, secret.expose().to_string()),
                Err(_) => {
                    return unknown(format!(
                        "the verify header name '{}' of {} is not valid; fix `verify.header` in \
                     agent-frameworks.toml.",
                        fw.verify.header, fw.label
                    ))
                }
            }
        }
    };
    let Ok(mut auth_header) = reqwest::header::HeaderValue::from_str(&auth_value) else {
        return VerifyResult {
            state: State::Invalid,
            detail: format!(
                "the stored {} login contains characters that cannot be sent in a request, so it \
                 cannot be valid; log in again.",
                fw.label
            ),
        };
    };
    auth_header.set_sensitive(true);
    headers.insert(auth_name, auth_header);

    let mut builder = reqwest::blocking::Client::builder()
        .timeout(timeout())
        .user_agent("ta-agent-login")
        // A redirect would carry the secret to another host.
        .redirect(reqwest::redirect::Policy::none());
    if is_loopback(&url) {
        builder = builder.no_proxy();
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => return unknown(format!("the HTTP client could not be set up: {e}")),
    };

    match client.get(url).headers(headers).send() {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if resp.status().is_success() {
                VerifyResult {
                    state: State::Ok,
                    detail: format!("{host} accepted the login (HTTP {status})."),
                }
            } else if fw.verify.invalid_status.contains(&status) {
                VerifyResult {
                    state: State::Invalid,
                    detail: format!(
                        "{host} rejected the {} login (HTTP {status}); log in again with \
                         `ta agent-login login {}`.",
                        fw.label, fw.id
                    ),
                }
            } else {
                unknown(format!(
                    "{host} answered HTTP {status}, which does not say whether the login works \
                     (rate limit, outage, or an endpoint that does not take this kind of login); \
                     try again later."
                ))
            }
        }
        Err(e) => {
            let e = e.without_url();
            let why = if e.is_timeout() {
                format!(
                    "no answer within {secs} s; raise {VERIFY_TIMEOUT_ENV} if the network is slow"
                )
            } else if e.is_connect() {
                "could not connect; check the network or proxy".to_string()
            } else {
                e.to_string()
            };
            unknown(format!(
                "{host} could not be reached to check the login ({why}); the stored login was \
                 kept. Run `ta agent-login verify {}` when the network is back.",
                fw.id
            ))
        }
    }
}
