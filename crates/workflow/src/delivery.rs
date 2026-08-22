//! SSH 目标与命令的公共类型（从 integrations re-export，领域边界内不暴露 SDK 细节）。
pub use sg_integrations::ssh::{
    validate_argv as validate_argv_pub, FakeSSH as FakeSSHPub, SSHAdapter as SSHAdapterPub,
    SSHCommand as SSHPubCommand, SSHTarget as SSHTargetPub,
};
