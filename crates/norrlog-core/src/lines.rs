//! Reading a log line by line, the one way every reader does it.
//!
//! The client writes whatever bytes a player typed or a server sent, and
//! not all of them are UTF-8. `BufRead::lines()` fails on the first such
//! line, and a reader that took that failure for the end of the file stopped
//! there and stored the half it had read as the whole log. Here a bad byte
//! becomes U+FFFD and the line is read like any other, which is what the
//! live follower has always done, so a log reads the same whichever path
//! reads it. A real I/O failure is still an error, never an early end.

use std::io::BufRead;

/// Hand each line of `reader` to `f`, without its CR/LF, decoding invalid
/// UTF-8 lossily. Returns the number of lines.
pub fn each_line(mut reader: impl BufRead, mut f: impl FnMut(&str)) -> std::io::Result<u64> {
    let mut buf = Vec::new();
    let mut n = 0u64;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            return Ok(n);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        f(&String::from_utf8_lossy(&buf));
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_byte_does_not_end_the_log() {
        let text: &[u8] = b"one\r\nbad: \xff\ntwo\nlast";
        let mut got = Vec::new();
        let n = each_line(text, |l| got.push(l.to_string())).unwrap();
        assert_eq!(n, 4);
        assert_eq!(got, ["one", "bad: \u{fffd}", "two", "last"]);
    }

    #[test]
    fn a_read_error_is_an_error_not_an_end() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk gone"))
            }
        }
        let r = each_line(std::io::BufReader::new(Broken), |_| {});
        assert!(r.is_err());
    }
}
