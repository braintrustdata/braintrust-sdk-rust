use crate::{BraintrustClient, DroppedRows};

/// A lightweight project facade sharing the SDK service and its parent's credentials.
/// Clones observe the same scoped work. The parent client's flush includes this project.
#[derive(Clone, Debug)]
pub struct Project {
    client: BraintrustClient,
    name: String,
}
impl Project {
    pub(crate) fn new(client: BraintrustClient, name: String) -> Self {
        Self { client, name }
    }
    /// The project name used for lazy resolution.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Wait for this handle's accepted project work, excluding sibling projects.
    #[must_use = "await or call blocking_wait to observe delivery"]
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        self.client.flush()
    }
    /// Dropped rows submitted through this project handle and its clones.
    pub fn dropped_rows(&self) -> DroppedRows {
        self.client.dropped_rows()
    }
}
impl std::ops::Deref for Project {
    type Target = BraintrustClient;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
