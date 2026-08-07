//! Splitting one file's text into stored chunks. Deterministic and dumb on
//! purpose: this is an archive lookup, not a retrieval-quality system, so a
//! fixed-size line window is enough and never needs tuning.

/// One piece of a file's content: a contiguous, 1-based, inclusive line
/// range and the text of exactly those lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub chunk_index: i64,
    pub start_line: i64,
    pub end_line: i64,
    pub text: String,
}

/// Lines per chunk. Chosen so a chunk is small enough to be a useful lookup
/// unit and large enough that a typical source file is a handful of chunks,
/// not hundreds.
pub const LINES_PER_CHUNK: usize = 200;

/// Split `text` into consecutive, non-overlapping windows of
/// `LINES_PER_CHUNK` lines. Empty input yields zero chunks (there is nothing
/// to find in an empty file). The final chunk may be shorter than the
/// window; nothing is padded or dropped.
pub fn chunk_text(text: &str) -> Vec<Chunk> {
    if text.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut chunks = Vec::new();
    let mut chunk_index = 0i64;
    for window_start in (0..lines.len()).step_by(LINES_PER_CHUNK) {
        let window_end = (window_start + LINES_PER_CHUNK).min(lines.len());
        let body = lines[window_start..window_end].join("\n");
        chunks.push(Chunk {
            chunk_index,
            start_line: (window_start + 1) as i64,
            end_line: window_end as i64,
            text: body,
        });
        chunk_index += 1;
    }
    chunks
}

/// Heuristic: does this blob look like text, or binary? Checked on the raw
/// bytes before anything is decoded, so a binary file (image, archive,
/// compiled artifact) never gets chunked into garbage text. Mirrors the
/// signal git itself uses (a NUL byte in the content), scanned over a
/// bounded prefix so a huge binary does not have to be scanned in full.
const BINARY_SNIFF_BYTES: usize = 8000;

pub fn looks_binary(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    prefix.contains(&0u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_produces_no_chunks() {
        assert!(chunk_text("").is_empty());
    }

    #[test]
    fn a_short_file_is_exactly_one_chunk_with_correct_line_bounds() {
        let text = "line1\nline2\nline3";
        let chunks = chunk_text(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert_eq!(chunks[0].text, text);
    }

    #[test]
    fn a_file_longer_than_the_window_splits_into_multiple_chunks_with_no_gap_or_overlap() {
        let lines: Vec<String> = (1..=450).map(|n| format!("line{n}")).collect();
        let text = lines.join("\n");
        let chunks = chunk_text(&text);
        assert_eq!(chunks.len(), 3, "450 lines at 200/chunk is 3 chunks (200, 200, 50)");
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 200);
        assert_eq!(chunks[1].start_line, 201);
        assert_eq!(chunks[1].end_line, 400);
        assert_eq!(chunks[2].start_line, 401);
        assert_eq!(chunks[2].end_line, 450);
        // no line duplicated across chunk boundaries
        assert!(chunks[0].text.ends_with("line200"));
        assert!(chunks[1].text.starts_with("line201"));
    }

    #[test]
    fn a_nul_byte_anywhere_in_the_sniff_window_is_detected_as_binary() {
        let mut bytes = b"normal text start".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(b"more text");
        assert!(looks_binary(&bytes));
    }

    #[test]
    fn plain_text_is_not_flagged_as_binary() {
        assert!(!looks_binary(b"fn main() {\n    println!(\"hi\");\n}\n"));
    }
}
