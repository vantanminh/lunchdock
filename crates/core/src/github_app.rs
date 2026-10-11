//! GitHub App JWT (RS256) used to mint an installation token.
//!
//! The private key stays with the caller. This function only returns the JWT.

use base64::Engine;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};

use crate::Error;

pub fn github_app_jwt(app_id: &str, private_key_pem: &str, now_unix: i64) -> Result<String, Error> {
    if app_id.is_empty() || app_id.len() > 20 || !app_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Validation("github app id must be numeric".into()));
    }
    let iat = now_unix.saturating_sub(60);
    let exp = now_unix.saturating_add(9 * 60);
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = format!(r#"{{"iat":{iat},"exp":{exp},"iss":"{app_id}"}}"#);
    let payload_b64 = b64url(payload.as_bytes());
    let signing_input = format!("{header}.{payload_b64}");
    let key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
        .map_err(|_| Error::Validation("github app private key could not be parsed".into()))?;
    let digest = Sha256::digest(signing_input.as_bytes());
    let signature = key
        .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
        .map_err(|_| Error::Validation("github app private key could not sign".into()))?;
    Ok(format!("{signing_input}.{}", b64url(&signature)))
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_signature_matches_the_app_key() {
        let pem = TEST_APP_KEY;
        let jwt = github_app_jwt("12345", pem, 1_700_000_000).unwrap();
        let mut parts = jwt.split('.');
        let header = parts.next().unwrap();
        let payload = parts.next().unwrap();
        let signature = parts.next().unwrap();
        assert!(parts.next().is_none());
        let signing_input = format!("{header}.{payload}");
        let signature_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .unwrap();
        let private = RsaPrivateKey::from_pkcs8_pem(pem).unwrap();
        private
            .to_public_key()
            .verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(signing_input.as_bytes()),
                &signature_bytes,
            )
            .unwrap();
        let payload_json = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .unwrap(),
        )
        .unwrap();
        assert!(payload_json.contains("\"iss\":\"12345\""));
        assert!(github_app_jwt("app", pem, 1_700_000_000).is_err());
    }

    const TEST_APP_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDMm3BWuPw2Hlpa
/TRK53FVFVMQAu4YEIgfe6UxBgWcDEIQK98obYe7y3D7+pwutdqcjjNpZiNX6mRx
VZqqMFwVfTDIKGIslUEIuJrwhGrVhXBiJFH32Lhi/N0a9elm4qKyvknri1ZnpORe
AneRnumaE4wX+euulDL+asQ0k6f/G9XVgMCHfS09dZisehNngSFFlyBoyG56gall
DvGQHqOUO1ewI5OS05G/7WmId5rbHAZ0k07jPlSHHf4OaaSWjlg3q4z1TiVAUuCc
/JW429HIqqiJTqu3oUQksf2G4jR7WsfLuEfkfqHM064WdjCVsaOjXHCDEa5fLtBC
MqAf+L73AgMBAAECggEACVnXQ1dhS3XiqkLlMhaBbaKJBnPOfty5OTsfkcMqhsxh
STeVaNXB/M6vmLcw48pINfHNHrWIyvqkNtvIiEZShww5f678zE3RSD1A84S8aeAX
LUe8qUecfFr9yJQm48la0dwe07wtVD+gd5LESoGelSiXNVH6N/h/m3S0sonHBUJ4
Tw14zOayx8ce1yxrHBob/MUt6GRS0YXjYxF8JvmntwwmzhHfWRbPGDDJgAns4v0P
XYdJWZ/f8QjXAYl6UY4aKzaG8kGznX/oOfNwBmLBHWrpMHoAgUBSxTzmfSee7zKw
6U5RwNLpEDz1JSSmcGO0JznQb8xo+naq32sPsWcC5QKBgQDtln3wmUVJwzNLe7x3
TGPgYrY0ZAVah7rdDjbg8d8aLFCmfjFavtUOiwtq0t6EFB1MA7qE335N8SX4ayrm
tRFt+H/GKF1CR8B2pBovaiYupshD8M9dFHm/v1giTJbcxzwNtAHPOjivRSzWhcr3
AdssvMNnH034fkK8RvuJi6vcuwKBgQDcdqSUJ4bZKoXif1ZvsA6sSngwwnzqrc/v
4njNhtweH1lY14RT1xm4J+piIIdU5MWhwrcRIlgAd9hAd22mMDMFOonwErOAOHID
7pIzjYHBmjJmISnSz5XjDMKJ+5eIS4y7qOwvUZjoGp4Ma5sthc7s1A2pid6CrZHy
1veESNOA9QKBgEMgBaz4YM8aS1/EMdcyyfUXbosWxzxwpKbX4UFklLf8VTz04lTV
HbTiK/OeaCfA1xYm54eq7TBwNV4eSRytg6MiVldSCCc1xna+nndO8kKz9FjdrHiL
1GoMUYY/RVBTKfBlLy2xMVqznJtvaYyoBZh6LGryOUE/yvMOyvptb70JAoGAVi2C
c+9nhOhcYYdJMGtvsOWDSfwcfFfpGmo6soxu6s9yS4ojTeqdQSFJO4ZfoE20+2Bz
0xzLrZMrUzfcAsSAjSCu3stQo2RtHSBawhAkkptF7Co2gRNoEdLhELLqLvrpTknf
2TdaQBB3i39cr1TI6EaT+JvOAOL8VCK3qlwVYSkCgYAFsXiAxrbo46C8cUFNqM/U
VX+MdJnueahwi5LKxcSztUwYLVSxrWk0e5/8wAhME36acfUl/0Xgf6SFs8iVbOJ6
RDhdlwzmNJrNLaFcSKAPVltO1B8fQcBxIVbQCOwyWawFdcRbwOUcHgmjY1Y+CRjY
ZSY+TIIzNCTcxml9MFZ09A==
-----END PRIVATE KEY-----
";
}
