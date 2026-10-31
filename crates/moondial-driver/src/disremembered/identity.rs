//! A passphrase per name. Set on first login, checked after, stored as an Argon2id hash
//! on the citizen's account component, which only admins can view.
//!
//! This is the minimum that lets a third person play without being able to play as you.
//! It is not an account system: no recovery, no email, no sessions beyond the connection.

use crate::world::World;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use moondial_store::EntityId;
use rand_core::OsRng;

/// Why a login was refused.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LoginError {
    /// Name exists and the passphrase does not match.
    #[error("{name} is already someone's, and that is not its passphrase. If the name is yours, send the passphrase it was set with; if not, choose another name (a passing name can collide with a live one, so an uncommon one is safer).")]
    WrongPassphrase { name: String },
    /// First login with a passphrase that is too short.
    #[error("A passphrase is at least {min} characters; it is set the first time a name logs in.")]
    TooShort { min: usize },
    /// A new name that is an old name in other letters.
    #[error("That name is taken, as {taken}. Log in as {taken}, or choose another name.")]
    Taken { taken: String },
    /// A passphrase is required.
    #[error("login <name> <passphrase>: the passphrase is set on first login and required after.")]
    Missing,
    /// Too many wrong passphrases for this name recently.
    #[error("Too many wrong passphrases for {name}; try again in {minutes} minutes.")]
    Locked { name: String, minutes: i64 },
    /// The world's fault.
    #[error("internal: {0}")]
    Internal(String),
}

pub const MIN_PASSPHRASE: usize = 8;

/// Reject a first-login passphrase that is empty or short.
pub fn acceptable(passphrase: &str) -> Result<(), LoginError> {
    if passphrase.is_empty() {
        return Err(LoginError::Missing);
    }
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(LoginError::TooShort {
            min: MIN_PASSPHRASE,
        });
    }
    Ok(())
}

/// Hash a passphrase for storage.
pub fn hash(passphrase: &str) -> Result<String, LoginError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(passphrase.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| LoginError::Internal(e.to_string()))
}

/// Check a passphrase against the citizen's stored hash.
pub fn check(world: &World, citizen: EntityId, passphrase: &str) -> Result<(), LoginError> {
    let name = super::plain_str(world, citizen, super::CITIZEN, "name")
        .unwrap_or("that name")
        .to_string();
    if passphrase.is_empty() {
        return Err(LoginError::Missing);
    }
    let Some(stored) = super::plain_str(world, citizen, super::ACCOUNT, "passphrase_hash") else {
        // A citizen from before passphrases existed: the first passphrase offered claims
        // the name. The pre-alpha world is reset before anyone else can.
        return Ok(());
    };
    let parsed = PasswordHash::new(stored).map_err(|e| LoginError::Internal(e.to_string()))?;
    Argon2::default()
        .verify_password(passphrase.as_bytes(), &parsed)
        .map_err(|_| LoginError::WrongPassphrase { name })
}
