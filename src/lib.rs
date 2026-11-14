pub mod analysis;
mod compiler;
mod diagnostic;
mod execution_context;
mod output;
mod project;

pub use compiler::compile_source;
pub use diagnostic::{CompileError, Diagnostic, SourceLocation};
pub use execution_context::ExecutionContext;
pub use project::{BuildError, BuildResult, Project, ProjectConfig};
