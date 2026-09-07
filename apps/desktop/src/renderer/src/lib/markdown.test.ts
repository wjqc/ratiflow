import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown XSS 防护', () => {
  it.each([
    '[点此](javascript:alert(document.domain))',
    '[点此](JaVaScRiPt:alert(1))',
    '[点此]( javascript:alert(1) )',
    '[点此](data:text/html;base64,PHNjcmlwdD4=)',
    '[点此](vbscript:msgbox(1))',
    '[点此](file:///etc/passwd)',
    '[点此](javascript&colon;alert(1))',
  ])('链接 scheme 白名单外降级为纯文本：%s', (md) => {
    const html = renderMarkdown(md);
    expect(html).not.toContain('<a ');
    expect(html.toLowerCase()).not.toContain('javascript:');
    expect(html.toLowerCase()).not.toContain('vbscript:');
    expect(html.toLowerCase()).not.toContain('data:text/html');
  });

  it.each(['![x](javascript:alert(1))', '![x](data:text/html,<script>)', '![x](file:///etc/passwd)'])(
    '图片 scheme 白名单外拒绝渲染：%s',
    (md) => {
      const html = renderMarkdown(md);
      expect(html).not.toContain('<img');
    },
  );

  it('http(s) 链接保留并加 rel 防护', () => {
    const html = renderMarkdown('[官网](https://example.com/a?b=1)');
    expect(html).toContain('<a href="https://example.com/a?b=1"');
    expect(html).toContain('rel="noopener noreferrer nofollow"');
    expect(html).toContain('target="_blank"');
  });

  it('mailto 链接保留', () => {
    const html = renderMarkdown('[信](mailto:a@example.com)');
    expect(html).toContain('<a href="mailto:a@example.com"');
  });

  it('位图 data URL 图片放行', () => {
    const html = renderMarkdown('![像素](data:image/png;base64,iVBORw0KGgo=)');
    expect(html).toContain('<img src="data:image/png;base64,iVBORw0KGgo="');
  });

  it('href 中的双引号被编码，不逃出属性', () => {
    const html = renderMarkdown('[x](https://a.com/?q="onmouseover="alert(1))');
    expect(html).toContain('%22');
    expect(html).not.toContain('q="onmouseover');
    expect(html).not.toContain('="alert');
  });

  it('原生 HTML 标签仍被转义（既有行为不回退）', () => {
    const html = renderMarkdown('<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>');
    expect(html).not.toContain('<script>');
    expect(html).not.toContain('<img');
  });

  it('autolink 的 http 链接可用', () => {
    const html = renderMarkdown('见 https://example.com docs');
    expect(html).toMatch(/<a href="https:\/\/example\.com"/);
  });
});
