//! Incremental parser for `text/event-stream` bodies (the WHATWG server-sent events format).

/// One dispatched server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, if any.
    pub event: Option<String>,
    /// All `data:` lines joined with `\n`.
    pub data: String,
}

/// Feeds arbitrary byte chunks and yields complete events. Lines may end in `\n`, `\r\n` or `\r`,
/// and may be split anywhere across chunks.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: String,
    has_data: bool,
    event: Option<String>,
    started: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes and returns the events completed by them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut start = 0;
        while let Some(off) = self.buf[start..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
        {
            let end = start + off;
            let next = match self.buf[end] {
                b'\r' if end + 1 == self.buf.len() => break,
                b'\r' if self.buf[end + 1] == b'\n' => end + 2,
                _ => end + 1,
            };
            let line = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
            start = next;
            if let Some(ev) = self.line(&line) {
                events.push(ev);
            }
        }
        self.buf.drain(..start);
        events
    }

    /// Flushes at end of stream. A trailing event without its blank line is still dispatched.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let rest = std::mem::take(&mut self.buf);
        let mut out = None;
        if !rest.is_empty() {
            let line = String::from_utf8_lossy(&rest);
            let line = line.strip_suffix('\r').unwrap_or(&line).to_owned();
            out = self.line(&line);
        }
        out.or_else(|| self.dispatch())
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        let line = if self.started {
            line
        } else {
            self.started = true;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        };
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.event = Some(value.to_owned()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        if !std::mem::take(&mut self.has_data) {
            return None;
        }
        Some(SseEvent {
            event,
            data: std::mem::take(&mut self.data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(events: &[SseEvent]) -> Vec<&str> {
        events.iter().map(|e| e.data.as_str()).collect()
    }

    #[test]
    fn parses_simple_events() {
        let mut p = SseParser::new();
        let ev = p.push(b"data: one\n\ndata: two\n\n");
        assert_eq!(data(&ev), ["one", "two"]);
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn handles_arbitrary_splits() {
        let input =
            "data: {\"a\":1}\r\n\r\n: keep-alive\n\nevent: x\ndata:no-space\n\ndata: [DONE]\n\n";
        let whole = SseParser::new().push(input.as_bytes());
        for split in 0..input.len() {
            let mut p = SseParser::new();
            let mut ev = p.push(&input.as_bytes()[..split]);
            ev.extend(p.push(&input.as_bytes()[split..]));
            assert_eq!(ev, whole, "split at {split}");
        }
        assert_eq!(data(&whole), ["{\"a\":1}", "no-space", "[DONE]"]);
        assert_eq!(whole[1].event.as_deref(), Some("x"));
    }

    #[test]
    fn byte_by_byte_with_multibyte_utf8() {
        let input = "data: Günaydın 🌅\n\n";
        let mut p = SseParser::new();
        let ev: Vec<_> = input
            .as_bytes()
            .iter()
            .flat_map(|b| p.push(&[*b]))
            .collect();
        assert_eq!(data(&ev), ["Günaydın 🌅"]);
    }

    #[test]
    fn joins_multiline_data_and_ignores_comments_and_unknown_fields() {
        let mut p = SseParser::new();
        let ev = p.push(b": comment\nid: 7\nretry: 100\ndata: a\ndata: b\nfoo: bar\n\n");
        assert_eq!(data(&ev), ["a\nb"]);
    }

    #[test]
    fn cr_only_line_endings() {
        let mut p = SseParser::new();
        let mut ev = p.push(b"data: a\r");
        assert!(ev.is_empty());
        ev.extend(p.push(b"\rdata: b\r\r"));
        // A trailing `\r` might be the start of `\r\n`, so it waits for more input or EOF.
        assert_eq!(data(&ev), ["a"]);
        ev.extend(p.push(b"data: c\r\n\r\n"));
        assert_eq!(data(&ev), ["a", "b", "c"]);
        let mut p = SseParser::new();
        p.push(b"data: d\r\r");
        assert_eq!(p.finish().map(|e| e.data), Some("d".into()));
    }

    #[test]
    fn blank_lines_without_data_dispatch_nothing() {
        let mut p = SseParser::new();
        assert!(p.push(b"\n\n\nevent: ping\n\n").is_empty());
    }

    #[test]
    fn empty_data_field_is_an_event() {
        let mut p = SseParser::new();
        assert_eq!(data(&p.push(b"data\n\n")), [""]);
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: tail").is_empty());
        assert_eq!(p.finish().map(|e| e.data), Some("tail".into()));
        let mut p = SseParser::new();
        assert!(p.push(b"data: tail\n").is_empty());
        assert_eq!(p.finish().map(|e| e.data), Some("tail".into()));
    }

    #[test]
    fn strips_leading_bom() {
        let mut p = SseParser::new();
        assert_eq!(data(&p.push("\u{feff}data: x\n\n".as_bytes())), ["x"]);
    }
}
