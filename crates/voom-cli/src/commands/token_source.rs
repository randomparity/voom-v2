use std::{env, fmt, io, io::Read, path::PathBuf};

use voom_core::ErrorCode;

#[derive(Debug, Clone)]
pub struct TokenSourceArgs {
    pub token_file: Option<PathBuf>,
    pub token_env: Option<String>,
    pub token_stdin: bool,
}

#[derive(Debug, Clone)]
pub enum TokenSourceError {
    BadArgs(String),
}

impl TokenSourceError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::BadArgs(_) => ErrorCode::BadArgs,
        }
    }
}

impl fmt::Display for TokenSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TokenSourceError {}

pub async fn read_token(args: &TokenSourceArgs) -> Result<String, TokenSourceError> {
    validate_token_source(args)?;

    let token = if let Some(path) = &args.token_file {
        tokio::fs::read_to_string(path).await.map_err(|err| {
            TokenSourceError::BadArgs(format!(
                "failed to read token file {}: {err}",
                path.display()
            ))
        })?
    } else if let Some(name) = &args.token_env {
        env::var(name).map_err(|err| {
            TokenSourceError::BadArgs(format!(
                "failed to read token environment variable {name}: {err}"
            ))
        })?
    } else {
        read_stdin_token().await?
    };

    let token = trim_one_trailing_newline(token);
    if token.is_empty() {
        return Err(TokenSourceError::BadArgs(
            "token source was empty".to_owned(),
        ));
    }
    Ok(token)
}

pub fn validate_token_source(args: &TokenSourceArgs) -> Result<(), TokenSourceError> {
    let source_count = usize::from(args.token_file.is_some())
        + usize::from(args.token_env.is_some())
        + usize::from(args.token_stdin);
    if source_count == 1 {
        Ok(())
    } else {
        Err(TokenSourceError::BadArgs(
            "pass exactly one token source".to_owned(),
        ))
    }
}

async fn read_stdin_token() -> Result<String, TokenSourceError> {
    tokio::task::spawn_blocking(|| {
        let mut token = String::new();
        io::stdin()
            .read_to_string(&mut token)
            .map(|_| token)
            .map_err(|err| {
                TokenSourceError::BadArgs(format!("failed to read token from stdin: {err}"))
            })
    })
    .await
    .map_err(|err| TokenSourceError::BadArgs(format!("failed to read token from stdin: {err}")))?
}

fn trim_one_trailing_newline(mut token: String) -> String {
    if token.ends_with("\r\n") {
        token.truncate(token.len() - 2);
    } else if token.ends_with('\n') {
        token.pop();
    }
    token
}

#[cfg(test)]
#[path = "token_source_test.rs"]
mod tests;
