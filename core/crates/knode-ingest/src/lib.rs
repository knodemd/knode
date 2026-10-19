//! Capture conversations from AI agents and import notes and chat exports.

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
