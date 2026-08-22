// seeddemo 向本地 data 目录预置演示项目 pj_demo 与一条示例工作项，
// 供 Web 控制台首次打开时有内容可看。用法：
//
//	go run ./scripts/seeddemo ./data
package main

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"time"

	_ "modernc.org/sqlite"
)

func main() {
	dataDir := "./data"
	if len(os.Args) > 1 {
		dataDir = os.Args[1]
	}
	db, err := sql.Open("sqlite", dataDir+"/sixgates.db")
	if err != nil {
		fatal("open", err)
	}
	defer db.Close()
	ctx := context.Background()

	stmts := []string{
		`INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
		 VALUES ('pj_demo','https://gitlab.example.com','team','demo','main', strftime('%Y-%m-%dT%H:%M:%fZ','now'))
		 ON CONFLICT(id) DO NOTHING`,
		`INSERT INTO workitems(id, project_id, gitlab_issue_iid, title, description, labels, current_gate, created_at, updated_at)
		 VALUES ('wi_demo_1','pj_demo','42','支持 SSO 登录','OIDC 单点登录，接入公司 IdP。','["feat"]','requirements',
		         strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))
		 ON CONFLICT(id) DO NOTHING`,
	}
	now := time.Now().UTC().Format("2006-01-02T15:04:05.000Z")
	for _, gate := range []string{"requirements", "design", "development", "testing", "deployment", "verification"} {
		state := "not_started"
		if gate == "requirements" {
			state = "running"
		}
		if _, err := db.ExecContext(ctx,
			`INSERT INTO workitem_stages(workitem_id, gate, state, input_baseline_sha, updated_at)
			 VALUES ('wi_demo_1', ?, ?, '', ?)
			 ON CONFLICT(workitem_id, gate) DO NOTHING`, gate, state, now); err != nil {
			fatal("seed stage "+gate, err)
		}
	}
	for _, stmt := range stmts {
		if _, err := db.ExecContext(ctx, stmt); err != nil {
			fatal("seed", err)
		}
	}
	fmt.Println("演示数据就绪：项目 pj_demo · 工作项 wi_demo_1（需求关 running）")
}

func fatal(stage string, err error) {
	fmt.Printf("%s: %v\n", stage, err)
	os.Exit(1)
}
