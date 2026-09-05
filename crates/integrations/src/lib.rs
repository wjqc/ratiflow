//! 外部系统适配器（v2 ADR-020 行为等价）：GitLab REST、OpenAI 兼容模型、SSH CLI。
//! SDK 细节只允许出现在本 crate；业务包只依赖 trait。

pub mod cancel;
pub mod gitlab;
pub mod mcp;
pub mod model;
pub mod sse;
pub mod ssh;
pub mod stream;
pub mod testflows;

pub use cancel::CancelToken;
pub use gitlab::{FakeGitLab, GitLabClient, GitLabHttp, GitLabIssue, GitLabMR, GitLabPipeline};
pub use model::{list_models, FakeModel, ModelHttp, ModelProvider};
pub use ssh::{FakeSSH, SSHAdapter, SSHTarget};
pub use stream::{
    NoopSink, StreamFuture, StreamSink, StreamUsage, StreamingModelProvider, SSE_IDLE,
    SSE_MAX_FRAME_BYTES, SSE_MAX_TOTAL_BYTES,
};
pub use testflows::{
    gitlab_test, model_test, ssh_target_test, HostKeyOutcome, StepResult, TestReport,
};
