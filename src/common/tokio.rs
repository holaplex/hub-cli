use anyhow::{Context, Result};

pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Error initializing async runtime")
}
