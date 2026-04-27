pub mod classifier;
pub mod extract;
pub mod spans;
pub mod split;

pub use classifier::{ByteClasses, ByteClassifier, DefaultClassifier, ScalarClassifier};
pub use extract::html::{extract_class_spans, extract_class_spans_with};
pub use spans::{ClassSpan, ClassToken};
pub use split::split_class_tokens;

#[cfg(target_arch = "x86_64")]
pub use classifier::RawSimdClassifier;
