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

/// Connect like the real client does (`src/client.rs`): retry on `NotFound`
/// while the server's accept loop races to create the next pipe instance.
/// Without this, a client reconnecting immediately after a previous
/// connection can land in the gap before `accept` re-creates an instance —
/// rare on a fast machine, routine on loaded CI runners.
fn connect_retry(name: &str) -> io::Result<PipeConn> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match PipeConn::connect(name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound && std::time::Instant::now() < deadline => {
                thread::sleep(std::time::Duration::from_millis(10));
            }
            other => return other,
        }
    }
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

/// Regression test for a real deadlock hit while building `src/server.rs`
/// (Task 6): a `try_clone`'d duplicate of a connection must support a
/// blocking read that never completes on ONE duplicate while a write
/// proceeds independently on ANOTHER duplicate of the same connection —
/// exactly the reader-thread / writer-thread-per-client shape the server
/// architecture requires. Before `pipe.rs` opened handles with
/// `FILE_FLAG_OVERLAPPED`, a pending synchronous `ReadFile` on one duplicate
/// serialized against (and forever blocked) a `WriteFile` on another
/// duplicate of the same underlying pipe object.
#[test]
fn write_on_one_clone_does_not_block_behind_a_pending_read_on_another() {
    let name = unique_pipe_name("clone-concurrency");
    let listener = PipeListener::bind(&name).expect("bind pipe");

    let server = thread::spawn(move || {
        let conn = listener.accept().expect("accept connection");
        let mut reader_conn = conn.try_clone().expect("clone for reader");
        let mut writer_conn = conn;

        // Reader half: consume one frame, then block forever waiting for a
        // second one that the client never sends.
        let reader = thread::spawn(move || {
            let _ = read_client_msg(&mut reader_conn).expect("read first frame");
            let _ = read_client_msg(&mut reader_conn); // blocks; thread is abandoned
        });

        // Give the reader thread time to be solidly parked in its second
        // (never-satisfied) read before attempting the write.
        thread::sleep(std::time::Duration::from_millis(200));
        write_server_msg(&mut writer_conn, &ServerMsg::Exit { code: 0, msg: "ok".to_string() })
            .expect("write must not block behind the other clone's pending read");

        let _ = reader; // never joined: it blocks forever by design
    });

    let mut client = PipeConn::connect(&name).expect("client connect");
    write_client_msg(&mut client, &ClientMsg::Stdin(vec![1, 2, 3])).expect("client write");
    let reply = read_server_msg(&mut client).expect("client read reply");
    assert_eq!(reply, ServerMsg::Exit { code: 0, msg: "ok".to_string() });

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

/// Double-autostart guard (Task 8 review fix): a SECOND `bind` on a name a
/// first listener already owns must fail (`FILE_FLAG_FIRST_PIPE_INSTANCE`
/// makes `CreateNamedPipeW` fail with `ERROR_ACCESS_DENIED`, surfacing as
/// `PermissionDenied`), and the first listener must remain fully usable —
/// this is what turns two racing cold-start clients into exactly one
/// surviving server instead of a split-brain pair sharing one pipe name.
#[test]
fn second_bind_same_name_fails() {
    let name = unique_pipe_name("double-bind");
    let listener = PipeListener::bind(&name).expect("first bind");

    let err = match PipeListener::bind(&name) {
        Ok(_) => panic!("second bind on an owned name must fail"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

    // The winner still accepts and serves a connection normally.
    let server = thread::spawn(move || {
        let mut conn = listener.accept().expect("accept after failed second bind");
        let msg = read_client_msg(&mut conn).expect("read client msg");
        match msg {
            ClientMsg::Stdin(bytes) => {
                write_server_msg(&mut conn, &ServerMsg::Output(bytes)).expect("write reply");
            }
            _ => panic!("unexpected message"),
        }
    });

    let mut client = PipeConn::connect(&name).expect("client connect");
    write_client_msg(&mut client, &ClientMsg::Stdin(vec![9, 9])).expect("client write");
    let reply = read_server_msg(&mut client).expect("client read");
    assert_eq!(reply, ServerMsg::Output(vec![9, 9]));

    drop(client);
    server.join().expect("server thread panicked");
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
        let mut client = connect_retry(&name).expect("client connect");
        let sent = vec![i; 3];
        write_client_msg(&mut client, &ClientMsg::Stdin(sent.clone())).expect("client write");
        let reply = read_server_msg(&mut client).expect("client read");
        assert_eq!(reply, ServerMsg::Output(sent));
        drop(client);
    }

    server.join().expect("server thread panicked");
}
