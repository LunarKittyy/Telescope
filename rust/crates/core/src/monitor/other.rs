//! Systems this isn't read on yet: every field is unknown.

use super::Sample;

pub(super) struct Reader;

impl Reader {
    pub fn new() -> Reader {
        Reader
    }

    pub fn sample(&mut self) -> Sample {
        Sample::default()
    }
}
