pub mod analysis;
mod compiler;
mod definitions;
mod diagnostic;
mod execution_context;
#[doc(hidden)]
pub mod output;
pub mod packages;
mod project;

pub use compiler::compile_source;
pub use diagnostic::{CompileError, Diagnostic, SourceLocation};
pub use execution_context::ExecutionContext;
pub use project::{BuildError, BuildResult, BuildSession, BuildStage, Project, ProjectConfig};

#[doc(hidden)]
pub use project::{BuildProgress, OutputChange};
