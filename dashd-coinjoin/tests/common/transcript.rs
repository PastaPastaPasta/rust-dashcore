//! Reader for the `cj1-v1` dashd CoinJoin transcripts.
//!
//! The capture format deliberately keeps protocol payloads as bytes.  The replay tests decode
//! only the messages they own, while this reader remains useful for facts and evidence which are
//! not part of a wire message (Core log lines, RPC snapshots, and observations).

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    pub name: String,
    pub format: String,
    pub mocktime: i64,
    pub ibd: bool,
    pub mask: Value,
    pub records: Vec<Record>,
    /// The fixture line of each record in `records`.
    pub lines: Vec<usize>,
    pub facts: BTreeMap<String, Value>,
    pub expects: Vec<Expectation>,
    pub logs: Vec<String>,
    pub observations: Vec<Observation>,
    pub rpcs: Vec<Rpc>,
    pub times: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Accept {
        peer: u32,
        suffix: Vec<String>,
    },
    Message(Message),
    Expect(Expectation),
    Log(String),
    Observe(Observation),
    Rpc(Rpc),
    Time(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub direction: Direction,
    pub peer: u32,
    pub command: String,
    pub payload: Vec<u8>,
    pub echo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToNode,
    FromNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub command: String,
    pub peer: u32,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rpc {
    pub method: String,
    pub params: Value,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

fn error(line: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        line,
        message: message.into(),
    }
}

fn json(line: usize, text: &str, what: &str) -> Result<Value, ParseError> {
    serde_json::from_str(text).map_err(|e| error(line, format!("invalid {what} JSON: {e}")))
}

fn parse_u32(line: usize, text: &str, what: &str) -> Result<u32, ParseError> {
    text.parse().map_err(|e| error(line, format!("invalid {what}: {e}")))
}

fn parse_i64(line: usize, text: &str, what: &str) -> Result<i64, ParseError> {
    text.parse().map_err(|e| error(line, format!("invalid {what}: {e}")))
}

fn parse_hex(line: usize, text: &str) -> Result<Vec<u8>, ParseError> {
    if !text.len().is_multiple_of(2) {
        return Err(error(line, "odd-length message payload"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|e| error(line, format!("invalid message payload: {e}")))
        })
        .collect()
}

fn object_field<'a>(line: usize, value: &'a Value, key: &str) -> Result<&'a Value, ParseError> {
    value.get(key).ok_or_else(|| error(line, format!("missing JSON field {key:?}")))
}

fn string_field(line: usize, value: &Value, key: &str) -> Result<String, ParseError> {
    object_field(line, value, key)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| error(line, format!("JSON field {key:?} is not a string")))
}

fn u32_field(line: usize, value: &Value, key: &str) -> Result<u32, ParseError> {
    object_field(line, value, key)?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| error(line, format!("JSON field {key:?} is not a u32")))
}

/// Parses one complete `cj1-v1` fixture.
pub fn parse(name: impl Into<String>, text: &str) -> Result<Fixture, ParseError> {
    let name = name.into();
    let mut format = None;
    let mut mocktime = None;
    let mut ibd = None;
    let mut mask = None;
    let mut records = Vec::new();
    let mut lines = Vec::new();
    let mut facts = BTreeMap::new();
    let mut expects = Vec::new();
    let mut logs = Vec::new();
    let mut observations = Vec::new();
    let mut rpcs = Vec::new();
    let mut times = Vec::new();

    for (line_index, raw) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.splitn(2, ' ');
        let kind = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        match kind {
            "format" => {
                if format.replace(rest.to_owned()).is_some() {
                    return Err(error(line_number, "duplicate format record"));
                }
            }
            "mocktime" => {
                if mocktime.replace(parse_i64(line_number, rest, "mocktime")?).is_some() {
                    return Err(error(line_number, "duplicate mocktime record"));
                }
            }
            "ibd" => {
                let value = match rest {
                    "0" => false,
                    "1" => true,
                    _ => return Err(error(line_number, "ibd must be 0 or 1")),
                };
                if ibd.replace(value).is_some() {
                    return Err(error(line_number, "duplicate ibd record"));
                }
            }
            "mask" => {
                if mask.replace(json(line_number, rest, "mask")?).is_some() {
                    return Err(error(line_number, "duplicate mask record"));
                }
            }
            "fact" => {
                let value = json(line_number, rest, "fact")?;
                let key = string_field(line_number, &value, "key")?;
                let fact = object_field(line_number, &value, "value")?.clone();
                if facts.insert(key.clone(), fact).is_some() {
                    return Err(error(line_number, format!("duplicate fact {key:?}")));
                }
            }
            "accept" => {
                let mut f = rest.split_whitespace();
                let peer = parse_u32(
                    line_number,
                    f.next().ok_or_else(|| error(line_number, "accept needs peer"))?,
                    "peer",
                )?;
                let suffix = f.map(ToOwned::to_owned).collect();
                let record = Record::Accept {
                    peer,
                    suffix,
                };
                records.push(record);
            }
            ">" | "<" => {
                let mut f = rest.split_whitespace();
                let peer = parse_u32(
                    line_number,
                    f.next().ok_or_else(|| error(line_number, "message needs peer"))?,
                    "peer",
                )?;
                let command =
                    f.next().ok_or_else(|| error(line_number, "message needs command"))?.to_owned();
                let payload_text = f.next().unwrap_or_default();
                let echo = payload_text == "echo";
                let payload = if echo {
                    Vec::new()
                } else {
                    parse_hex(line_number, payload_text)?
                };
                if f.next().is_some() {
                    return Err(error(line_number, "message has trailing fields"));
                }
                records.push(Record::Message(Message {
                    direction: if kind == ">" {
                        Direction::ToNode
                    } else {
                        Direction::FromNode
                    },
                    peer,
                    command,
                    payload,
                    echo,
                }));
            }
            "expect" => {
                let value = json(line_number, rest, "expect")?;
                let expectation = Expectation {
                    command: string_field(line_number, &value, "command")?,
                    peer: u32_field(line_number, &value, "peer")?,
                    value: object_field(line_number, &value, "value")?.clone(),
                };
                expects.push(expectation.clone());
                records.push(Record::Expect(expectation));
            }
            "log" => {
                let value = json(line_number, rest, "log")?;
                let log = value
                    .as_str()
                    .ok_or_else(|| error(line_number, "log must be a JSON string"))?
                    .to_owned();
                logs.push(log.clone());
                records.push(Record::Log(log));
            }
            "observe" => {
                let value = json(line_number, rest, "observe")?;
                let observation = Observation {
                    key: string_field(line_number, &value, "key")?,
                    value: object_field(line_number, &value, "value")?.clone(),
                };
                observations.push(observation.clone());
                records.push(Record::Observe(observation));
            }
            "rpc" => {
                let value = json(line_number, rest, "rpc")?;
                let rpc = Rpc {
                    method: string_field(line_number, &value, "method")?,
                    params: object_field(line_number, &value, "params")?.clone(),
                    result: object_field(line_number, &value, "result")?.clone(),
                };
                rpcs.push(rpc.clone());
                records.push(Record::Rpc(rpc));
            }
            "time" => {
                let time = parse_i64(line_number, rest, "time")?;
                times.push(time);
                records.push(Record::Time(time));
            }
            other => return Err(error(line_number, format!("unknown record kind {other:?}"))),
        }
        lines.resize(records.len(), line_number);
    }

    let format = format.ok_or_else(|| error(0, "missing format record"))?;
    if format != "cj1-v1" {
        return Err(error(0, format!("unsupported format {format:?}")));
    }
    Ok(Fixture {
        name,
        format,
        mocktime: mocktime.ok_or_else(|| error(0, "missing mocktime record"))?,
        ibd: ibd.ok_or_else(|| error(0, "missing ibd record"))?,
        mask: mask.ok_or_else(|| error(0, "missing mask record"))?,
        records,
        lines,
        facts,
        expects,
        logs,
        observations,
        rpcs,
        times,
    })
}

#[allow(dead_code)]
pub fn fixture(name: &str) -> Fixture {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    parse(name, &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}
