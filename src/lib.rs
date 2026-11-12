mod compiler;
mod diagnostic;
mod output;
mod project;

pub use compiler::compile_source;
pub use diagnostic::{CompileError, Diagnostic, SourceLocation};
pub use project::{BuildError, BuildResult, Project, ProjectConfig};
