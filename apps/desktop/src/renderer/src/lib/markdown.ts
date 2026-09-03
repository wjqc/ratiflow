import { marked } from 'marked';

// 渲染前转义原生 HTML（内容来自 Agent/用户输入，不信任内嵌标签），再交 marked 解析。
marked.setOptions({ gfm: true, breaks: true });
export function renderMarkdown(content: string): string {
  const escaped = content.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  return marked.parse(escaped, { async: false }) as string;
}
