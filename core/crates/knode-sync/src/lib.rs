//! Sync spaces between devices with Loro updates.

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_a_version() {
        assert!(!super::version().is_empty());
    }
}
