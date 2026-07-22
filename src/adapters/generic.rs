use crate::adapters::LanguageAdapter;
use crate::repo::RepoKind;

#[derive(Debug, Clone, Copy)]
pub struct GenericAdapter;

impl LanguageAdapter for GenericAdapter {
    fn kind(&self) -> RepoKind {
        RepoKind::Generic
    }
}
