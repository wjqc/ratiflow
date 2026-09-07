import { marked } from 'marked';
import type { Tokens } from 'marked';

// 渲染前转义原生 HTML（内容来自 Agent/用户输入，不信任内嵌标签），再交 marked 解析。
marked.setOptions({ gfm: true, breaks: true });

// 链接与图片地址走 scheme 白名单：javascript:/data:/file:/vbscript: 等一律降级为纯文本。
// 内容来自模型输出与知识库，视为不可信输入（XSS：渲染层可经 window.sixgates.rpc 调用任意 core RPC）。
function safeHref(raw: string | undefined): string | null {
  const url = (raw ?? '').trim();
  if (/^(https?:\/\/|mailto:)/i.test(url)) {
    return url.replace(/"/g, '%22');
  }
  return null;
}

function safeImageSrc(raw: string | undefined): string | null {
  const url = (raw ?? '').trim();
  if (/^(https?:\/\/)/i.test(url)) {
    return url.replace(/"/g, '%22');
  }
  // 仅放行位图类 data URL；data:text/html 等一律拒绝。
  if (/^data:image\/(png|jpeg|gif|webp|avif);base64,[a-z0-9+/=]+$/i.test(url)) {
    return url;
  }
  return null;
}

function attr(raw: string): string {
  return raw.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

const renderer = {
  link(this: { parser: { parseInline(tokens: Tokens.Generic[]): string } }, { href, title, tokens }: Tokens.Link): string {
    const text = this.parser.parseInline(tokens);
    const safe = safeHref(href);
    if (safe === null) {
      return text;
    }
    const titleAttr = title ? ` title="${attr(title)}"` : '';
    return `<a href="${safe}"${titleAttr} target="_blank" rel="noopener noreferrer nofollow">${text}</a>`;
  },
  image(this: { parser: { parseInline(tokens: Tokens.Generic[]): string } }, { href, title, text }: Tokens.Image): string {
    const safe = safeImageSrc(href);
    if (safe === null) {
      return `<span class="sg-md-img-blocked">${attr(text || '图片已拦截')}</span>`;
    }
    const titleAttr = title ? ` title="${attr(title)}"` : '';
    return `<img src="${safe}" alt="${attr(text)}"${titleAttr} loading="lazy" />`;
  },
};

marked.use({ renderer });

export function renderMarkdown(content: string): string {
  const escaped = content.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  return marked.parse(escaped, { async: false }) as string;
}
