use std::io::{self, ErrorKind, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_HEADER_LEN: u32 = 16 * 1024 * 1024;
const MAX_PAYLOAD_LEN: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcHeader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct IpcFrame {
    pub header: IpcHeader,
    pub payload: Vec<u8>,
}

impl IpcFrame {
    pub fn request(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            header: IpcHeader {
                id: Some(id),
                method: Some(method.into()),
                params: Some(params),
                ok: None,
                result: None,
                error: None,
                event: None,
            },
            payload: Vec::new(),
        }
    }

    pub fn ok(id: u64, result: Value, payload: Vec<u8>) -> Self {
        Self {
            header: IpcHeader {
                id: Some(id),
                method: None,
                params: None,
                ok: Some(true),
                result: Some(result),
                error: None,
                event: None,
            },
            payload,
        }
    }

    pub fn error(id: Option<u64>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            header: IpcHeader {
                id,
                method: None,
                params: None,
                ok: Some(false),
                result: None,
                error: Some(IpcError {
                    code: code.into(),
                    message: message.into(),
                }),
                event: None,
            },
            payload: Vec::new(),
        }
    }

    pub fn event(event: impl Into<String>, params: Value) -> Self {
        Self {
            header: IpcHeader {
                id: None,
                method: None,
                params: Some(params),
                ok: None,
                result: None,
                error: None,
                event: Some(event.into()),
            },
            payload: Vec::new(),
        }
    }
}

pub fn write_frame(mut writer: impl Write, frame: &IpcFrame) -> anyhow::Result<()> {
    let header = serde_json::to_vec(&frame.header)?;
    anyhow::ensure!(
        header.len() <= MAX_HEADER_LEN as usize,
        "IPC header is too large"
    );
    anyhow::ensure!(
        frame.payload.len() as u64 <= MAX_PAYLOAD_LEN,
        "IPC payload is too large"
    );
    writer.write_u32::<LittleEndian>(header.len() as u32)?;
    writer.write_u64::<LittleEndian>(frame.payload.len() as u64)?;
    writer.write_all(&header)?;
    writer.write_all(&frame.payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame(mut reader: impl Read) -> anyhow::Result<Option<IpcFrame>> {
    let header_len = match reader.read_u32::<LittleEndian>() {
        Ok(len) => len,
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let payload_len = reader.read_u64::<LittleEndian>()?;
    anyhow::ensure!(
        header_len <= MAX_HEADER_LEN,
        "IPC header length is too large"
    );
    anyhow::ensure!(
        payload_len <= MAX_PAYLOAD_LEN,
        "IPC payload length is too large"
    );

    let mut header = vec![0u8; header_len as usize];
    reader.read_exact(&mut header)?;
    let mut payload = vec![0u8; payload_len as usize];
    reader.read_exact(&mut payload)?;
    Ok(Some(IpcFrame {
        header: serde_json::from_slice(&header)?,
        payload,
    }))
}

pub fn params_as<T: for<'de> Deserialize<'de>>(frame: &IpcFrame) -> anyhow::Result<T> {
    let params = frame.header.params.clone().unwrap_or(Value::Null);
    Ok(serde_json::from_value(params)?)
}

pub fn result_as<T: for<'de> Deserialize<'de>>(frame: &IpcFrame) -> anyhow::Result<T> {
    if frame.header.ok != Some(true) {
        if let Some(error) = &frame.header.error {
            anyhow::bail!("{}: {}", error.code, error.message);
        }
        anyhow::bail!("IPC response was not successful");
    }
    let result = frame.header.result.clone().unwrap_or(Value::Null);
    Ok(serde_json::from_value(result)?)
}

pub fn error_from_anyhow(err: anyhow::Error) -> IpcError {
    IpcError {
        code: "internal_error".to_owned(),
        message: format!("{err:#}"),
    }
}

pub fn broken_pipe(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .map(|err| {
            matches!(
                err.kind(),
                ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_roundtrip() {
        let frame = IpcFrame::request(7, "search.query", json!({"query": "foo"}));
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).unwrap();
        let decoded = read_frame(&bytes[..]).unwrap().unwrap();
        assert_eq!(decoded.header.id, Some(7));
        assert_eq!(decoded.header.method.as_deref(), Some("search.query"));
    }

    #[test]
    fn rejects_truncated_frame() {
        let frame = IpcFrame::ok(1, json!({"ok": true}), b"payload".to_vec());
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).unwrap();
        bytes.truncate(bytes.len() - 2);
        assert!(read_frame(&bytes[..]).is_err());
    }
}
