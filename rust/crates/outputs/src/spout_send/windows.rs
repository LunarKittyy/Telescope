//! Spout send on Windows: a Spout sender for each shared stream. Not made yet.

use super::Backend;
use std::sync::Arc;

pub fn backend() -> Option<Arc<dyn Backend>> {
    None
}
