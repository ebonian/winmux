//! Client/server frame codec for the named-pipe transport.
//!
//! Pure logic, no I/O beyond `std::io::{Read, Write}` (so it works over a
//! named pipe, a TCP socket, or an in-memory `Vec<u8>` cursor in tests).
//!
//! Wire format (see `docs/specs/2026-07-07-server-client-design.md`,
//! "Transport"): `[type: u8][len: u32 LE][payload: len bytes]`, max `len`
//! `MAX_FRAME` (1 MiB) — larger declared lengths are a protocol error.
//! Strings are UTF-8 with a `u16` length prefix, except `CliDone`'s
//! `out`/`err` which use a `u32` length prefix (they carry command output,
//! not names).

use std::io::{self, Read, Write};

pub const MAX_FRAME: u32 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    Attach {
        mode: AttachMode,
        detach_others: bool,
        cols: u16,
        rows: u16,
        name: String,
    },
    Stdin(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Detach,
    Cli(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachMode {
    Existing = 0,
    NewNamed = 1,
    NewAuto = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    Output(Vec<u8>),
    Exit { code: u8, msg: String },
    CliDone { code: u8, out: String, err: String },
}

// Frame type bytes (see wire format tables in the design spec).
const T_ATTACH: u8 = 0x01;
const T_STDIN: u8 = 0x02;
const T_RESIZE: u8 = 0x03;
const T_DETACH: u8 = 0x04;
const T_CLI: u8 = 0x05;
const T_OUTPUT: u8 = 0x81;
const T_EXIT: u8 = 0x82;
const T_CLIDONE: u8 = 0x83;

fn invalid_data(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

// ---- low-level frame write/read -------------------------------------------

fn write_frame(w: &mut impl Write, ty: u8, payload: &[u8]) -> io::Result<()> {
    w.write_all(&[ty])?;
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    Ok(())
}

/// Read one frame's type byte and payload. EOF on the very first read (no
/// bytes available for the type byte) or a short read anywhere in the frame
/// surfaces as `read_exact`'s `ErrorKind::UnexpectedEof`.
fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut ty = [0u8; 1];
    r.read_exact(&mut ty)?;

    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(invalid_data("frame length exceeds MAX_FRAME"));
    }

    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    Ok((ty[0], payload))
}

// ---- payload cursor helpers -------------------------------------------

/// Take `n` bytes off the front of `buf`, advancing it. A short buffer is a
/// malformed payload (declared field lengths don't fit what's there).
fn take<'a>(buf: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    if buf.len() < n {
        return Err(invalid_data("truncated field in payload"));
    }
    let (head, tail) = buf.split_at(n);
    *buf = tail;
    Ok(head)
}

fn read_u8(buf: &mut &[u8]) -> io::Result<u8> {
    Ok(take(buf, 1)?[0])
}

fn read_u16(buf: &mut &[u8]) -> io::Result<u16> {
    let b = take(buf, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(buf: &mut &[u8]) -> io::Result<u32> {
    let b = take(buf, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_str_u16(buf: &mut &[u8]) -> io::Result<String> {
    let len = read_u16(buf)? as usize;
    let bytes = take(buf, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid_data("invalid utf8"))
}

fn read_str_u32(buf: &mut &[u8]) -> io::Result<String> {
    let len = read_u32(buf)? as usize;
    let bytes = take(buf, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid_data("invalid utf8"))
}

// ---- ClientMsg --------------------------------------------------------

pub fn write_client_msg(w: &mut impl Write, m: &ClientMsg) -> io::Result<()> {
    match m {
        ClientMsg::Attach {
            mode,
            detach_others,
            cols,
            rows,
            name,
        } => {
            let mut payload = Vec::new();
            payload.push(*mode as u8);
            payload.push(*detach_others as u8);
            payload.extend_from_slice(&cols.to_le_bytes());
            payload.extend_from_slice(&rows.to_le_bytes());
            let name_bytes = name.as_bytes();
            payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(name_bytes);
            write_frame(w, T_ATTACH, &payload)
        }
        ClientMsg::Stdin(bytes) => write_frame(w, T_STDIN, bytes),
        ClientMsg::Resize { cols, rows } => {
            let mut payload = Vec::with_capacity(4);
            payload.extend_from_slice(&cols.to_le_bytes());
            payload.extend_from_slice(&rows.to_le_bytes());
            write_frame(w, T_RESIZE, &payload)
        }
        ClientMsg::Detach => write_frame(w, T_DETACH, &[]),
        ClientMsg::Cli(args) => {
            let mut payload = Vec::new();
            payload.extend_from_slice(&(args.len() as u16).to_le_bytes());
            for a in args {
                let b = a.as_bytes();
                payload.extend_from_slice(&(b.len() as u16).to_le_bytes());
                payload.extend_from_slice(b);
            }
            write_frame(w, T_CLI, &payload)
        }
    }
}

pub fn read_client_msg(r: &mut impl Read) -> io::Result<ClientMsg> {
    let (ty, payload) = read_frame(r)?;
    let mut buf = &payload[..];
    match ty {
        T_ATTACH => {
            let mode_byte = read_u8(&mut buf)?;
            let mode = match mode_byte {
                0 => AttachMode::Existing,
                1 => AttachMode::NewNamed,
                2 => AttachMode::NewAuto,
                _ => return Err(invalid_data("unknown AttachMode")),
            };
            let detach_others = read_u8(&mut buf)? != 0;
            let cols = read_u16(&mut buf)?;
            let rows = read_u16(&mut buf)?;
            let name = read_str_u16(&mut buf)?;
            Ok(ClientMsg::Attach {
                mode,
                detach_others,
                cols,
                rows,
                name,
            })
        }
        T_STDIN => Ok(ClientMsg::Stdin(payload)),
        T_RESIZE => {
            let cols = read_u16(&mut buf)?;
            let rows = read_u16(&mut buf)?;
            Ok(ClientMsg::Resize { cols, rows })
        }
        T_DETACH => Ok(ClientMsg::Detach),
        T_CLI => {
            let argc = read_u16(&mut buf)?;
            let mut args = Vec::with_capacity(argc as usize);
            for _ in 0..argc {
                args.push(read_str_u16(&mut buf)?);
            }
            Ok(ClientMsg::Cli(args))
        }
        _ => Err(invalid_data("unknown ClientMsg type")),
    }
}

// ---- ServerMsg --------------------------------------------------------

pub fn write_server_msg(w: &mut impl Write, m: &ServerMsg) -> io::Result<()> {
    match m {
        ServerMsg::Output(bytes) => write_frame(w, T_OUTPUT, bytes),
        ServerMsg::Exit { code, msg } => {
            let mut payload = Vec::new();
            payload.push(*code);
            let msg_bytes = msg.as_bytes();
            payload.extend_from_slice(&(msg_bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(msg_bytes);
            write_frame(w, T_EXIT, &payload)
        }
        ServerMsg::CliDone { code, out, err } => {
            let mut payload = Vec::new();
            payload.push(*code);
            let out_bytes = out.as_bytes();
            payload.extend_from_slice(&(out_bytes.len() as u32).to_le_bytes());
            payload.extend_from_slice(out_bytes);
            let err_bytes = err.as_bytes();
            payload.extend_from_slice(&(err_bytes.len() as u32).to_le_bytes());
            payload.extend_from_slice(err_bytes);
            write_frame(w, T_CLIDONE, &payload)
        }
    }
}

pub fn read_server_msg(r: &mut impl Read) -> io::Result<ServerMsg> {
    let (ty, payload) = read_frame(r)?;
    let mut buf = &payload[..];
    match ty {
        T_OUTPUT => Ok(ServerMsg::Output(payload)),
        T_EXIT => {
            let code = read_u8(&mut buf)?;
            let msg = read_str_u16(&mut buf)?;
            Ok(ServerMsg::Exit { code, msg })
        }
        T_CLIDONE => {
            let code = read_u8(&mut buf)?;
            let out = read_str_u32(&mut buf)?;
            let err = read_str_u32(&mut buf)?;
            Ok(ServerMsg::CliDone { code, out, err })
        }
        _ => Err(invalid_data("unknown ServerMsg type")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn attach_roundtrip() {
        let msg = ClientMsg::Attach {
            mode: AttachMode::NewNamed,
            detach_others: true,
            cols: 120,
            rows: 40,
            name: "work".to_string(),
        };
        let mut buf = Vec::new();
        write_client_msg(&mut buf, &msg).unwrap();
        let got = read_client_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn stdin_roundtrip() {
        let msg = ClientMsg::Stdin(vec![1, 2, 3, 0, 255]);
        let mut buf = Vec::new();
        write_client_msg(&mut buf, &msg).unwrap();
        let got = read_client_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn resize_roundtrip() {
        let msg = ClientMsg::Resize { cols: 200, rows: 60 };
        let mut buf = Vec::new();
        write_client_msg(&mut buf, &msg).unwrap();
        let got = read_client_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn detach_roundtrip() {
        let msg = ClientMsg::Detach;
        let mut buf = Vec::new();
        write_client_msg(&mut buf, &msg).unwrap();
        let got = read_client_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn cli_roundtrip() {
        let msg = ClientMsg::Cli(vec![
            "new-session".to_string(),
            "-s".to_string(),
            "main".to_string(),
        ]);
        let mut buf = Vec::new();
        write_client_msg(&mut buf, &msg).unwrap();
        let got = read_client_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn output_roundtrip() {
        let msg = ServerMsg::Output(vec![0x1b, b'[', b'2', b'J']);
        let mut buf = Vec::new();
        write_server_msg(&mut buf, &msg).unwrap();
        let got = read_server_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn exit_roundtrip() {
        let msg = ServerMsg::Exit {
            code: 1,
            msg: "[exited]".to_string(),
        };
        let mut buf = Vec::new();
        write_server_msg(&mut buf, &msg).unwrap();
        let got = read_server_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn clidone_roundtrip() {
        let msg = ServerMsg::CliDone {
            code: 0,
            out: "main: 1 windows (created ...)\n".to_string(),
            err: String::new(),
        };
        let mut buf = Vec::new();
        write_server_msg(&mut buf, &msg).unwrap();
        let got = read_server_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(got, msg);
    }

    // Byte-exact golden test: Attach{Existing, false, 80, 24, "main"}.
    //
    // Payload is mode(1) + detach_others(1) + cols(2) + rows(2) +
    // name_len(2) + name(4 bytes "main") = 12 bytes, so the u32 LE length
    // prefix is 12 (the task brief's literal text said 11, which
    // undercounts the listed payload bytes by one — see task-1-report.md).
    #[test]
    fn attach_wire_bytes() {
        let msg = ClientMsg::Attach {
            mode: AttachMode::Existing,
            detach_others: false,
            cols: 80,
            rows: 24,
            name: "main".to_string(),
        };
        let mut buf = Vec::new();
        write_client_msg(&mut buf, &msg).unwrap();
        assert_eq!(
            buf,
            vec![
                0x01, 12, 0, 0, 0, 0x00, 0x00, 0x50, 0x00, 0x18, 0x00, 0x04, 0x00, b'm', b'a',
                b'i', b'n'
            ]
        );
    }

    #[test]
    fn unknown_type_is_invalid_data() {
        // type 0xff, len 0, no payload.
        let buf = vec![0xffu8, 0, 0, 0, 0];
        let err = read_client_msg(&mut Cursor::new(buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversize_len_is_invalid_data() {
        // A valid client type with a declared length over MAX_FRAME.
        let mut buf = vec![T_STDIN];
        buf.extend_from_slice(&(MAX_FRAME + 1).to_le_bytes());
        let err = read_client_msg(&mut Cursor::new(buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_payload_is_eof() {
        // Declares a 10-byte payload but only supplies 3 bytes before EOF.
        let mut buf = vec![T_STDIN];
        buf.extend_from_slice(&10u32.to_le_bytes());
        buf.extend_from_slice(&[1, 2, 3]);
        let err = read_client_msg(&mut Cursor::new(buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
