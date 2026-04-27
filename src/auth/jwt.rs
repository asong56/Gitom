use anyhow::Result;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// Minimal claims: only expiry. The token simply proves "the holder is authenticated".
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub exp: i64,
}

pub fn sign(secret: &str, expire_hours: i64) -> Result<(String, i64)> {
    let now        = time::OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(expire_hours);
    let exp        = expires_at.unix_timestamp();

    let claims = Claims { exp };
    let token  = encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))?;
    Ok((token, exp))
}

pub fn verify(token: &str, secret: &str) -> Result<Claims> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(data.claims)
}
