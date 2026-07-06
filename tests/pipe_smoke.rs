//! Integration smoke tests for the named-pipe transport (`src/pipe.rs`).
//! These create real Windows named pipes, so they only run on Windows.
//! Pipe names include the process id (plus a per-test tag) so repeated or
//! parallel test runs never collide.

use std::io;
use std::thread;

use winmux::pipe::{PipeConn, PipeListener};
use winmux::protocol::{read_client_msg, read_server_msg, write_client_msg, write_server_msg};
use winmux::protocol::{ClientMsg, ServerMsg};

fn unique_pipe_name(tag: &str) -> String {
    format!(r"\\.\pipe\winmux-test-pipe-{}-{}", std::process::id(), tag)
}

/// A client connects, sends a `ClientMsg::Stdin` frame, and the server echoes
/// it back as a `ServerMsg::Output` frame — exercising Task 1's codec over a
/// real named pipe end to end.
#[test]
fn roundtrip_client_server() {
    let name = unique_pipe_name("roundtrip");
    let listener = PipeListener::bind(&name).expect("bind pipe");

    let server = thread::spawn(move || {
        let mut conn = listener.accept().expect("accept connection");
        loop {
            match read_client_msg(&mut conn) {
                Ok(ClientMsg::Stdin(bytes)) => {
                    write_server_msg(&mut conn, &ServerMsg::Output(bytes))
                        .expect("write reply");
                }
                Ok(_) => panic!("unexpected message"),
                Err(_) => break, // client disconnected
            }
        }
    });

    let mut client = PipeConn::connect(&name).expect("client connect");
    let sent = vec![1u8, 2, 3, 4, 5];
    write_client_msg(&mut client, &ClientMsg::Stdin(sent.clone())).expect("client write");
    let reply = read_server_msg(&mut client).expect("client read");
    assert_eq!(reply, ServerMsg::Output(sent));

    drop(client);
    server.join().expect("server thread panicked");
}

/// Connecting to a pipe nobody has bound must surface as `NotFound` — the
/// CLI's "no server running" signal.
#[test]
fn connect_absent_pipe_is_not_found() {
    let name = unique_pipe_name("absent");
    let err = PipeConn::connect(&name).expect_err("connect to absent pipe must fail");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

/// The accept loop must serve one connection after another over the same
/// listener (first instance, then a freshly created instance).
#[test]
fn two_sequential_clients() {
    let name = unique_pipe_name("sequential");
    let listener = PipeListener::bind(&name).expect("bind pipe");

    let server = thread::spawn(move || {
        for _ in 0..2 {
            let mut conn = listener.accept().expect("accept connection");
            let msg = read_client_msg(&mut conn).expect("read client msg");
            match msg {
                ClientMsg::Stdin(bytes) => {
                    write_server_msg(&mut conn, &ServerMsg::Output(bytes))
                        .expect("write reply");
                }
                _ => panic!("unexpected message"),
            }
        }
    });

    for i in 0..2u8 {
        let mut client = PipeConn::connect(&name).expect("client connect");
        let sent = vec![i; 3];
        write_client_msg(&mut client, &ClientMsg::Stdin(sent.clone())).expect("client write");
        let reply = read_server_msg(&mut client).expect("client read");
        assert_eq!(reply, ServerMsg::Output(sent));
        drop(client);
    }

    server.join().expect("server thread panicked");
}
