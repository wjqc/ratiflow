//! 增量 SSE 解析与可取消流式泵（ADR-033 M2）。
//!
//! 设计要点（方案 §5 M2 测试清单驱动）：
//! - 字节级增量解析：帧/行边界按 `\n` `\r\n` 识别，半个 UTF-8 序列留缓冲等待后续
//!   chunk，只在完整行上解码——TCP 分片绝不撕裂码点。
//! - 帧大小上限（防止单帧撑爆内存）与流累计上限（计量+滥用防护）在泵层强制。
//! - 取消经 [`CancelToken`]：select 在每个 chunk 边界触发，放弃 future 即关闭
//!   reqwest 连接（socket 随 drop 关闭），实现"取消 1 秒内终止 socket"。
//! - 读空闲 watchdog：Provider 停止发字节超过 idle 时长按流中断处理，不挂死 Run。

use std::time::Duration;

use crate::cancel::CancelToken;

/// 单条 SSE 帧（event 字段 + 合并后的 data；多行 data 以 \n 连接）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

/// 流式泵统计（计量入 rollout/model_turns）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SsePumpStats {
    /// 服务端累计发送的字节数（含 SSE 帧 LSP 开销）。
    pub total_bytes: u64,
    /// 完整帧数。
    pub frames: u64,
    /// 是否见到流正常结束哨兵（EOF 或调用方确认完成帧由 adapter 判定）。
    pub saw_eof: bool,
}

/// 增量 SSE 解析器：`feed` 任意分片的字节，吐出完整帧。
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    data_lines: Vec<Vec<u8>>,
    saw_field: bool,
    total_bytes: u64,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// 喂入字节，返回新完成的帧。max_frame_bytes 限制单帧（含缓冲中的未完成帧）
    /// 字节数，超限即协议错误——调用方必须终止流。
    pub fn feed(&mut self, chunk: &[u8], max_frame_bytes: usize) -> Result<Vec<SseFrame>, String> {
        self.total_bytes += chunk.len() as u64;
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > max_frame_bytes {
            return Err(format!(
                "model_protocol_violation: SSE 帧超过 {max_frame_bytes} 字节上限"
            ));
        }
        let mut frames = Vec::new();
        loop {
            // 找下一个完整行终止符（\n 或 \r；\r\n 在下一轮吃掉 \n）。
            let Some(pos) = self.buf.iter().position(|&b| b == b'\n' || b == b'\r') else {
                if self.buf.len() > max_frame_bytes {
                    return Err(format!(
                        "model_protocol_violation: SSE 帧超过 {max_frame_bytes} 字节上限"
                    ));
                }
                break;
            };
            let terminator = self.buf[pos];
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // 去掉 \n 或 \r
            if terminator == b'\r' && self.buf.first() == Some(&b'\n') {
                // \r\n 成对：吃掉 \r 后残留的 \n，避免被误判为空行帧边界。
                self.buf.drain(..1);
            }
            if line.is_empty() {
                // 空行 = 帧边界：派发累积帧。
                if self.saw_field {
                    let data = String::from_utf8(self.data_lines.join(&b'\n'))
                        .map_err(|_| "model_protocol_violation: SSE 帧含非法 UTF-8".to_string())?;
                    frames.push(SseFrame {
                        event: std::mem::take(&mut self.event),
                        data,
                    });
                }
                self.event.clear();
                self.data_lines.clear();
                self.saw_field = false;
            } else {
                self.saw_field = true;
                self.ingest_line(&line);
            }
        }
        Ok(frames)
    }

    /// EOF 收尾：派发无终止空行的残余帧（容忍服务器不发结尾空行）。
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, String> {
        let mut frames = Vec::new();
        if self.buf.iter().any(|&b| b == b'\n' || b == b'\r') {
            return Err("model_protocol_violation: SSE 缓冲残缺".into());
        }
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.saw_field = true;
            self.ingest_line(&line);
        }
        if self.saw_field {
            let data = String::from_utf8(self.data_lines.join(&b'\n'))
                .map_err(|_| "model_protocol_violation: SSE 帧含非法 UTF-8".to_string())?;
            frames.push(SseFrame {
                event: std::mem::take(&mut self.event),
                data,
            });
            self.event.clear();
            self.data_lines.clear();
            self.saw_field = false;
        }
        Ok(frames)
    }

    fn ingest_line(&mut self, line: &[u8]) {
        // 注释行（: 开头）忽略；field 形如 "data:"/"data: xxx"/"event: xxx"。
        if line.first() == Some(&b':') {
            return;
        }
        let (name, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => (line, &line[..0]),
        };
        let value = if value.first() == Some(&b' ') {
            &value[1..]
        } else {
            value
        };
        match name {
            b"event" => {
                if let Ok(s) = std::str::from_utf8(value) {
                    self.event = s.to_string();
                }
            }
            b"data" => self.data_lines.push(value.to_vec()),
            // id/retry 等字段与 chat 协议无关，忽略。
            _ => {}
        }
    }
}

/// 泵错误分类（错误串前缀即协议错误码，调用方按前缀分流）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsePumpError {
    Cancelled,
    Interrupted(String),
    Violation(String),
    Transport(String),
}

impl SsePumpError {
    pub fn to_error_string(self) -> String {
        match self {
            SsePumpError::Cancelled => "model_cancelled".into(),
            SsePumpError::Interrupted(d) => format!("model_stream_interrupted: {d}"),
            SsePumpError::Violation(d) => format!("model_protocol_violation: {d}"),
            SsePumpError::Transport(d) => format!("model_unavailable: {d}"),
        }
    }
}

/// 驱动一个已建立的 SSE 响应直到 EOF/取消/错误。每个 frame 回调一次 on_frame；
/// 回调返回 Err 即终止（错误码原样传播）。回调须 Send（整个泵是 Send future）。
pub async fn pump_sse(
    response: reqwest::Response,
    cancel: &CancelToken,
    max_frame_bytes: usize,
    max_total_bytes: usize,
    idle: Duration,
    on_frame: &mut (dyn FnMut(SseFrame) -> Result<(), SsePumpError> + Send),
) -> Result<SsePumpStats, SsePumpError> {
    use futures_util::StreamExt;
    let mut parser = SseParser::new();
    let mut stats = SsePumpStats::default();
    let mut stream = response.bytes_stream();
    loop {
        let chunk = {
            let next = stream.next();
            tokio::select! {
                _ = cancel.cancelled() => return Err(SsePumpError::Cancelled),
                r = tokio::time::timeout(idle, next) => match r {
                    Err(_) => {
                        return Err(SsePumpError::Interrupted(format!(
                            "读取空闲超过 {idle:?}，按流中断处理"
                        )));
                    }
                    Ok(None) => None,
                    Ok(Some(Err(e))) => {
                        if cancel.is_cancelled() {
                            return Err(SsePumpError::Cancelled);
                        }
                        return Err(SsePumpError::Transport(format!("流读取失败: {e}")));
                    }
                    Ok(Some(Ok(bytes))) => Some(bytes),
                },
            }
        };
        match chunk {
            None => {
                stats.saw_eof = true;
                for frame in parser.finish().map_err(SsePumpError::Violation)? {
                    stats.frames += 1;
                    on_frame(frame)?;
                }
                return Ok(stats);
            }
            Some(bytes) => {
                if parser.total_bytes() + bytes.len() as u64 > max_total_bytes as u64 {
                    return Err(SsePumpError::Violation(format!(
                        "SSE 流超过 {max_total_bytes} 字节累计上限"
                    )));
                }
                let frames = parser.feed(&bytes, max_frame_bytes).map_err(|d| {
                    if d.starts_with("model_protocol_violation") {
                        SsePumpError::Violation(
                            d.trim_start_matches("model_protocol_violation: ").into(),
                        )
                    } else {
                        SsePumpError::Violation(d)
                    }
                })?;
                for frame in frames {
                    stats.frames += 1;
                    on_frame(frame)?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(chunks: &[&[u8]]) -> Result<Vec<SseFrame>, String> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.feed(c, 1 << 20)?);
        }
        out.extend(p.finish()?);
        Ok(out)
    }

    #[test]
    fn single_chunk_complete_stream() {
        let body = b"event: delta\ndata: {\"a\":1}\n\ndata: [DONE]\n\n";
        let fs = frames(&[body]).unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].event, "delta");
        assert_eq!(fs[0].data, "{\"a\":1}");
        assert_eq!(fs[1].data, "[DONE]");
    }

    #[test]
    fn byte_split_across_chunks_including_utf8_half() {
        // 中文"你"是 3 字节（E4 BD A0）：故意在码点中间切断。
        let mut full = b"data: {\"text\":\"\xE4\xBD\xA0\xE5\xA5\xBD\"}\n\n".to_vec();
        let mid = full.len() - 4; // 切在最后一个 UTF-8 序列中间
        let (a, b) = full.split_at(mid);
        let a = a.to_vec();
        let b = b.to_vec();
        full.clear();
        let fs = frames(&[&a, &b]).unwrap();
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].data, "{\"text\":\"你好\"}");
    }

    #[test]
    fn crlf_and_multiline_data() {
        let body = b"data: line1\r\ndata: line2\r\n\r\ndata: x\r\n\r\n";
        let fs = frames(&[body]).unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].data, "line1\nline2");
        assert_eq!(fs[1].data, "x");
    }

    #[test]
    fn comment_lines_and_field_without_space() {
        let body = b": keep-alive\ndata:no-space\n\ndata: z\n\n";
        let fs = frames(&[body]).unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].data, "no-space");
    }

    #[test]
    fn eof_without_terminator_flushes_last_frame() {
        let fs = frames(&[b"data: tail"]).unwrap();
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].data, "tail");
    }

    #[test]
    fn oversized_frame_is_violation() {
        let mut p = SseParser::new();
        let big = vec![b'x'; 4096];
        let err = p.feed(&big, 1024).unwrap_err();
        assert!(err.contains("model_protocol_violation"), "{err}");
    }

    #[test]
    fn invalid_utf8_in_frame_is_violation() {
        let err = frames(&[b"data: \xFF\xFE\n\n"]).unwrap_err();
        assert!(err.contains("UTF-8"), "{err}");
    }
}
