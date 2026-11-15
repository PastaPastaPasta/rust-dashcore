//! The Core-linked judge (`../bs8-judge`): Dash Core's own reads, one line
//! per case.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::Value;

use crate::predict::{Case, Read};

/// A running judge process.
pub struct Judge {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Judge {
    pub fn spawn(path: &str) -> Self {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("judge {path}: {e}"));
        let stdin = BufWriter::new(child.stdin.take().expect("judge stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("judge stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Core's read of `case`: what it read and wrote back, or its exception.
    pub fn read(&mut self, case: &Case) -> Result<Read, String> {
        let raw = if case.raw.is_empty() {
            "-".to_string()
        } else {
            hex::encode(&case.raw)
        };
        writeln!(
            self.stdin,
            "{} {} {} {raw} {} {}",
            case.cmd,
            case.version,
            u8::from(case.legacy),
            case.max_a,
            case.max_b
        )
        .and_then(|()| self.stdin.flush())
        .expect("judge stdin");
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("judge stdout");
        assert!(n > 0, "the judge exited");
        let v: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("judge output {line:?}: {e}"));
        if v["ok"] != Value::Bool(true) {
            return Err(v["error"].as_str().unwrap_or("?").to_string());
        }
        Ok(Read {
            consumed: v["consumed"].as_u64().expect("consumed") as usize,
            reenc: hex::decode(v["reenc"].as_str().expect("reenc")).expect("reenc hex"),
            error_code: v["error_code"].as_u64().map(|c| c as u8),
            vvec_size: v["vvec_size"].as_u64().map(|c| c as usize),
            hash: v["hash"].as_str().map(|h| hex::decode(h).expect("hash hex")),
        })
    }
}

impl Drop for Judge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
