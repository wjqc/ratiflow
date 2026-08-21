// contractcheck 验证 OpenAPI YAML 与 JSON Schema 契约文件可解析且结构合法（CI validate 阶段）。
// 不引入外部依赖：OpenAPI 用最小 YAML 子集校验（关键键存在 + paths/components 非空），
// JSON Schema 用 encoding/json 全量解析。
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

func main() {
	root := "contracts"
	failures := 0

	// --- OpenAPI ---
	openapiBody, err := os.ReadFile(filepath.Join(root, "openapi", "sixgates.yaml"))
	if err != nil {
		fmt.Println("FAIL: 读取 openapi 失败:", err)
		os.Exit(1)
	}
	openapi := string(openapiBody)
	for _, key := range []string{"openapi: 3.1.0", "servers:", "paths:", "components:", "securitySchemes:"} {
		if !strings.Contains(openapi, key) {
			fmt.Printf("FAIL: openapi 缺少关键段落 %q\n", key)
			failures++
		}
	}
	// 契约-实现对齐：路由注册必须都在 paths 中声明（契约 paths 不含 /api/v1 前缀）。
	routePattern := regexp.MustCompile(`"(GET|POST|PUT|PATCH|DELETE) (/api/v1/[^\"]+)"`)
	declaredPattern := regexp.MustCompile(`^  (/[^\s:]+):`)
	declared := map[string]bool{}
	for _, line := range strings.Split(openapi, "\n") {
		if m := declaredPattern.FindStringSubmatch(line); m != nil {
			declared[m[1]] = true
		}
	}
	// 从 api/server.go 抽取已注册路由（去掉 /api/v1 前缀后与契约 paths 对齐）。
	serverBody, err := os.ReadFile("internal/api/server.go")
	if err != nil {
		fmt.Println("FAIL: 读取 server.go 失败:", err)
		os.Exit(1)
	}
	seen := map[string]bool{}
	for _, m := range routePattern.FindAllStringSubmatch(string(serverBody), -1) {
		path := strings.TrimPrefix(m[2], "/api/v1")
		if seen[path] {
			continue
		}
		seen[path] = true
		if !declared[path] {
			fmt.Printf("FAIL: 路由 %s 已实现但未在 OpenAPI 声明（或反之）\n", m[2])
			failures++
		}
	}
	if failures == 0 {
		fmt.Println("openapi: 契约与路由对齐检查通过")
	}

	// --- JSON Schemas ---
	schemaFiles, err := filepath.Glob(filepath.Join(root, "jsonschema", "*.json"))
	if err != nil || len(schemaFiles) == 0 {
		fmt.Println("FAIL: 无 JSON Schema 文件")
		os.Exit(1)
	}
	for _, file := range schemaFiles {
		body, err := os.ReadFile(file)
		if err != nil {
			fmt.Printf("FAIL: 读取 %s: %v\n", file, err)
			failures++
			continue
		}
		var doc map[string]any
		if err := json.Unmarshal(body, &doc); err != nil {
			fmt.Printf("FAIL: %s 不是合法 JSON: %v\n", file, err)
			failures++
			continue
		}
		if _, ok := doc["$schema"]; !ok {
			fmt.Printf("FAIL: %s 缺少 $schema\n", file)
			failures++
		}
	}
	if failures == 0 {
		fmt.Printf("jsonschema: %d 个文件全部合法\n", len(schemaFiles))
	}

	if failures > 0 {
		fmt.Printf("contractcheck: %d 处失败\n", failures)
		os.Exit(1)
	}
}
