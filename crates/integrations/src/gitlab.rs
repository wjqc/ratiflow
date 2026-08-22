//! GitLab REST API v4 适配器 + fake（MR head 漂移、pipeline 乱序、权限不足契约场景）。
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub trait GitLabClient: Send + Sync {
    fn current_user(&self) -> Result<Value, String>;
    fn get_issue(&self, project_id: &str, iid: &str) -> Result<GitLabIssue, String>;
    fn create_mr(
        &self,
        project_id: &str,
        source: &str,
        target: &str,
        title: &str,
    ) -> Result<GitLabMR, String>;
    fn get_mr(&self, project_id: &str, iid: &str) -> Result<GitLabMR, String>;
    fn pipelines_for_ref(
        &self,
        project_id: &str,
        gitlab_ref: &str,
    ) -> Result<Vec<GitLabPipeline>, String>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabIssue {
    pub iid: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMR {
    pub iid: String,
    pub source_branch: String,
    pub target_branch: String,
    pub sha: String,
    pub state: String,
    pub web_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabPipeline {
    pub id: i64,
    pub sha: String,
    #[serde(rename = "ref")]
    pub gitlab_ref: String,
    pub status: String,
}

pub struct GitLabHttp {
    pub base_url: String,
    pub token: String,
}

impl GitLabHttp {
    fn get(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}/api/v4{}", self.base_url.trim_end_matches('/'), path);
        let response = ureq::get(&url)
            .set("Private-Token", &self.token)
            .timeout(std::time::Duration::from_secs(15))
            .call()
            .map_err(|e| format!("gitlab_unreachable: {e}"))?;
        if response.status() == 401 || response.status() == 403 {
            return Err("gitlab_forbidden: token 权限不足".into());
        }
        if response.status() >= 400 {
            return Err(format!("gitlab_error: HTTP {}", response.status()));
        }
        response.into_json::<Value>().map_err(|e| e.to_string())
    }
}

impl GitLabClient for GitLabHttp {
    fn current_user(&self) -> Result<Value, String> {
        self.get("/user")
    }

    fn get_issue(&self, project_id: &str, iid: &str) -> Result<GitLabIssue, String> {
        let raw = self.get(&format!("/projects/{project_id}/issues/{iid}"))?;
        Ok(GitLabIssue {
            iid: raw["iid"]
                .as_i64()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            title: raw["title"].as_str().unwrap_or_default().into(),
            body: raw["description"].as_str().unwrap_or_default().into(),
            labels: raw["labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            state: raw["state"].as_str().unwrap_or_default().into(),
        })
    }

    fn create_mr(
        &self,
        _project_id: &str,
        _source: &str,
        _target: &str,
        _title: &str,
    ) -> Result<GitLabMR, String> {
        Err("gitlab_mr_create: 未配置真实实例（诊断页检查 SIXGATES_GITLAB_URL/TOKEN）".into())
    }

    fn get_mr(&self, project_id: &str, iid: &str) -> Result<GitLabMR, String> {
        let raw = self.get(&format!("/projects/{project_id}/merge_requests/{iid}"))?;
        Ok(GitLabMR {
            iid: raw["iid"]
                .as_i64()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            source_branch: raw["source_branch"].as_str().unwrap_or_default().into(),
            target_branch: raw["target_branch"].as_str().unwrap_or_default().into(),
            sha: raw["sha"].as_str().unwrap_or_default().into(),
            state: raw["state"].as_str().unwrap_or_default().into(),
            web_url: raw["web_url"].as_str().unwrap_or_default().into(),
        })
    }

    fn pipelines_for_ref(
        &self,
        project_id: &str,
        gitlab_ref: &str,
    ) -> Result<Vec<GitLabPipeline>, String> {
        let raw = self.get(&format!(
            "/projects/{project_id}/pipelines?ref={gitlab_ref}"
        ))?;
        let list = raw.as_array().cloned().unwrap_or_default();
        Ok(list
            .iter()
            .map(|p| GitLabPipeline {
                id: p["id"].as_i64().unwrap_or_default(),
                sha: p["sha"].as_str().unwrap_or_default().into(),
                gitlab_ref: p["ref"].as_str().unwrap_or_default().into(),
                status: p["status"].as_str().unwrap_or_default().into(),
            })
            .collect())
    }
}

/// 内存 fake：契约测试用。
#[derive(Default)]
pub struct FakeGitLab {
    pub permission_denied: bool,
    pub issues: std::collections::HashMap<String, GitLabIssue>,
    pub mrs: Vec<GitLabMR>,
    pub pipelines: Vec<GitLabPipeline>,
}

impl GitLabClient for FakeGitLab {
    fn current_user(&self) -> Result<Value, String> {
        if self.permission_denied {
            return Err("gitlab_forbidden".into());
        }
        Ok(serde_json::json!({"id": 1, "username": "tester"}))
    }

    fn get_issue(&self, project_id: &str, iid: &str) -> Result<GitLabIssue, String> {
        self.issues
            .get(&format!("{project_id}/{iid}"))
            .cloned()
            .ok_or_else(|| "gitlab_not_found".into())
    }

    fn create_mr(
        &self,
        project_id: &str,
        source: &str,
        target: &str,
        title: &str,
    ) -> Result<GitLabMR, String> {
        let iid = (self.mrs.len() + 1).to_string();
        let mr = GitLabMR {
            iid: iid.clone(),
            source_branch: source.into(),
            target_branch: target.into(),
            sha: format!("sha-mr-{iid}"),
            state: "opened".into(),
            web_url: format!("https://gitlab.test/{project_id}/-/merge_requests/{iid}"),
        };
        let _ = title;
        Ok(mr)
    }

    fn get_mr(&self, _project_id: &str, iid: &str) -> Result<GitLabMR, String> {
        self.mrs
            .iter()
            .find(|m| m.iid == iid)
            .cloned()
            .ok_or_else(|| "gitlab_not_found".into())
    }

    fn pipelines_for_ref(
        &self,
        _project_id: &str,
        gitlab_ref: &str,
    ) -> Result<Vec<GitLabPipeline>, String> {
        Ok(self
            .pipelines
            .iter()
            .filter(|p| p.gitlab_ref == gitlab_ref)
            .cloned()
            .collect())
    }
}
