// One set of lexing rules per dialect, shared by the run splitter, the editor and the read-only classifier.
pub(crate) mod boundary;
pub mod prim;
pub mod rules;

pub use rules::{HashComment, LexRules};
