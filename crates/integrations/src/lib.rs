//! 外部系统适配器（v2 ADR-020 行为等价）：GitLab REST、OpenAI 兼容模型、SSH CLI。
//! SDK 细节只允许出现在本 crate；业务包只依赖 trait。

pub mod gitlab;
pub mod model;
pub mod ssh;
pub mod testflows;

pub use gitlab::{FakeGitLab, GitLabClient, GitLabHttp, GitLabIssue, GitLabMR, GitLabPipeline};
pub use model::{list_models, FakeModel, ModelHttp, ModelProvider};
pub use ssh::{FakeSSH, SSHAdapter, SSHTarget};
pub use testflows::{
    gitlab_test, model_test, ssh_target_test, HostKeyOutcome, StepResult, TestReport,
};
