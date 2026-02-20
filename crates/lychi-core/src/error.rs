use thiserror::Error;

#[derive(Error, Debug)]
pub enum LychiError {
    #[error("Unknown command: {0}")]
    UnknownCommand(String),

    #[error("Command execution failed: {0}")]
    ExecutionFailed(String),

    #[error("App not found: {0}")]
    AppNotFound(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("History error: {0}")]
    History(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("AI error: {0}")]
    Ai(String),

    #[error("Notes error: {0}")]
    Notes(String),

    #[error("Database error: {0}")]
    Database(String),
}

impl From<redb::DatabaseError> for LychiError {
    fn from(e: redb::DatabaseError) -> Self {
        LychiError::Database(e.to_string())
    }
}

impl From<redb::StorageError> for LychiError {
    fn from(e: redb::StorageError) -> Self {
        LychiError::Database(e.to_string())
    }
}

impl From<redb::TransactionError> for LychiError {
    fn from(e: redb::TransactionError) -> Self {
        LychiError::Database(e.to_string())
    }
}

impl From<redb::TableError> for LychiError {
    fn from(e: redb::TableError) -> Self {
        LychiError::Database(e.to_string())
    }
}

impl From<redb::CommitError> for LychiError {
    fn from(e: redb::CommitError) -> Self {
        LychiError::Database(e.to_string())
    }
}

impl serde::Serialize for LychiError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
