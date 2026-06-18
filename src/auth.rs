//! Resolves the credentials for rwire's HTTP Basic gate (`Server::basic_auth`),
//! which rejects unauthenticated requests — page and WebSocket alike — before any
//! UI is served. The password comes from `CLAW_AUTH_TOKEN`; if unset we mint one
//! for the run and print it, so the server is never silently open on the LAN.

use crate::id::random_hex;

const USER_ENV: &str = "CLAW_AUTH_USER";
const TOKEN_ENV: &str = "CLAW_AUTH_TOKEN";
const DEFAULT_USER: &str = "claw";

pub struct Credentials {
    pub user: String,
    pub password: String,
    /// The password was minted because none was configured.
    pub generated: bool,
}

pub fn resolve() -> std::io::Result<Credentials> {
    let user = std::env::var(USER_ENV).unwrap_or_else(|_| DEFAULT_USER.to_owned());
    match std::env::var(TOKEN_ENV) {
        Ok(password) => Ok(Credentials {
            user,
            password,
            generated: false,
        }),
        Err(_) => Ok(Credentials {
            user,
            password: random_hex(16)?,
            generated: true,
        }),
    }
}
