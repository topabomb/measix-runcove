use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid Authorization header format")]
    InvalidHeader,
}

pub fn extract_bearer_token(header: &str) -> Result<&str, AuthError> {
    let parts: Vec<&str> = header.splitn(2, ' ').collect();
    if parts.len() != 2 || !parts[0].eq_ignore_ascii_case("bearer") {
        return Err(AuthError::InvalidHeader);
    }
    Ok(parts[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bearer_header_parsing() {
        for (header, expected) in [("Bearer abc123", "abc123"), ("bearer token123", "token123")] {
            assert_eq!(extract_bearer_token(header).unwrap(), expected);
        }
        for header in ["", "Basic abc", "Bearer"] {
            assert!(matches!(
                extract_bearer_token(header),
                Err(AuthError::InvalidHeader)
            ));
        }
    }
}
