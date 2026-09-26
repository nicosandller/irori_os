//! Shared types and utilities for the Irori UI crate.

/// Copy button feedback message type. Holds optional feedback string that appears temporarily after copying.
#[derive(Clone, PartialEq)]
pub struct CopyFeedback(pub Option<String>);

impl CopyFeedback {
    /// Create a new `CopyFeedback` without any message.
    pub fn none() -> Self {
        CopyFeedback(None)
    }

    /// Create a new `CopyFeedback` with the given message.
    pub fn new(msg: impl Into<String>) -> Self {
        CopyFeedback(Some(msg.into()))
    }
}

impl std::fmt::Display for CopyFeedback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(msg) = self.0.as_ref() {
            write!(f, "{}", msg)
        } else {
            Ok(())
        }
    }
}

impl Default for CopyFeedback {
    fn default() -> Self {
        CopyFeedback::none()
    }
}
