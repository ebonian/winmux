//! tmux command tokenizer, command table, and typed commands.
//!
//! Pure module: no I/O, `std` only. This is the parsing layer shared by all
//! four SP3 command entry points (`.tmux.conf` lines, the `winmux` CLI
//! argv, the `prefix-:` command prompt, and key bindings) — see the `## cmd`
//! section of `docs/specs/2026-07-07-command-config-interfaces.md`.
//!
//! Pipeline: `join_continuations` (file loader: physical lines -> logical
//! lines) -> [`parse_line`] (a logical line -> one or more [`RawCmd`]s,
//! tmux config tokenization) -> [`resolve`] (a `RawCmd` -> a typed
//! [`ParsedCmd`] via the command table: full names + tmux aliases + flag
//! parsing). Execution against live server state is a later task (Task 6) —
//! this module only parses.

use crate::geom::Direction;

/// One untyped command: a name (already alias-resolved-or-not — that's
/// [`resolve`]'s job) plus its argument tokens, exactly as produced by
/// [`parse_line`]'s tokenizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCmd {
    pub name: String,
    pub args: Vec<String>,
}

/// Tokenize one logical config/prompt/CLI line into zero or more [`RawCmd`]s.
///
/// Rules (tmux config-line tokenization):
/// - Whitespace (space/tab, and stray `\r`/`\n`) splits tokens and is
///   otherwise discarded.
/// - `'...'` is a literal run: no escapes recognized inside, including `#`
///   and `;` (both lose all special meaning between the quotes).
/// - `"..."` recognizes exactly two escapes: `\"` -> literal `"`, `\\` ->
///   literal `\`. Any other `\<c>` is passed through verbatim as BOTH
///   characters (the backslash is not an escape for anything else).
/// - A quoted segment can be adjacent to bare characters or another quoted
///   segment with no intervening whitespace — they concatenate into one
///   token (shell-style), e.g. `foo'bar'"baz"` is one token `foobarbaz`.
/// - `#` outside any quote starts a comment that runs to the end of the
///   line (even mid-token: `foo#bar` tokenizes `foo` then discards `#bar`).
/// - An unquoted, unescaped `;` is a command separator: it ends the token in
///   progress and the current command, and starts a new one. It is never
///   itself part of any token.
/// - `\;` outside quotes is an escaped semicolon: the backslash is consumed
///   and a literal `;` is appended to the token being built (this does
///   *not* end the command — see `resolve`'s tail-splitting for
///   `bind-key`/`confirm-before`, which is where a lone `\;` token, or an
///   equivalently-lone quoted `";"`, actually takes effect).
/// - Outside quotes, a bare `\` not followed by `;` is literal (no other
///   escape sequences are recognized at the top level).
///
/// Returns `Err("unterminated quote")` if a `'` or `"` is never closed
/// before the line ends. Empty commands (blank line, comment-only line, or
/// runs of bare `;`) are silently dropped, not emitted as empty `RawCmd`s.
pub fn parse_line(line: &str) -> Result<Vec<RawCmd>, String> {
    let mut commands: Vec<RawCmd> = Vec::new();
    let mut cur_tokens: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut chars = line.chars().peekable();

    fn push_token(token: &mut Option<String>, cur_tokens: &mut Vec<String>) {
        if let Some(t) = token.take() {
            cur_tokens.push(t);
        }
    }
    fn push_command(cur_tokens: &mut Vec<String>, commands: &mut Vec<RawCmd>) {
        if !cur_tokens.is_empty() {
            let name = cur_tokens.remove(0);
            commands.push(RawCmd { name, args: std::mem::take(cur_tokens) });
        }
    }

    while let Some(c) = chars.next() {
        match c {
            '#' => break,
            ';' => {
                push_token(&mut token, &mut cur_tokens);
                push_command(&mut cur_tokens, &mut commands);
            }
            ' ' | '\t' | '\r' | '\n' => {
                push_token(&mut token, &mut cur_tokens);
            }
            '\'' => {
                let buf = token.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => buf.push(ch),
                        None => return Err("unterminated quote".to_string()),
                    }
                }
            }
            '"' => {
                let buf = token.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('"') => buf.push('"'),
                            Some('\\') => buf.push('\\'),
                            Some(other) => {
                                buf.push('\\');
                                buf.push(other);
                            }
                            None => return Err("unterminated quote".to_string()),
                        },
                        Some(ch) => buf.push(ch),
                        None => return Err("unterminated quote".to_string()),
                    }
                }
            }
            '\\' if chars.peek() == Some(&';') => {
                chars.next();
                token.get_or_insert_with(String::new).push(';');
            }
            other => {
                token.get_or_insert_with(String::new).push(other);
            }
        }
    }
    push_token(&mut token, &mut cur_tokens);
    push_command(&mut cur_tokens, &mut commands);
    Ok(commands)
}

/// Join `\`-continued physical lines into logical lines for the config file
/// loader. A physical line whose last character is a single trailing `\`
/// (after stripping an optional trailing `\r`, so CRLF files work) is
/// joined directly (backslash removed, no separator inserted) with the next
/// physical line; this repeats across a chain of continuations. Returns
/// `(first_line_number, joined_text)` pairs, 1-based, one per logical line.
///
/// A trailing `\` on the very last physical line (nothing left to join with)
/// has no continuation to perform, so the backslash is put back rather than
/// silently dropped.
pub fn join_continuations<'a, I: Iterator<Item = &'a str>>(lines: I) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (idx, raw) in lines.enumerate() {
        let lineno = idx + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let (start, acc) = match pending.take() {
            Some((n, s)) => (n, s + line),
            None => (lineno, line.to_string()),
        };
        match acc.strip_suffix('\\') {
            Some(stripped) => pending = Some((start, stripped.to_string())),
            None => out.push((start, acc)),
        }
    }
    if let Some((n, s)) = pending {
        // Nothing followed to continue onto — the trailing backslash was
        // never actually a continuation, so keep it.
        out.push((n, format!("{s}\\")));
    }
    out
}

/// A fully typed, alias-resolved SP3 command. `bind-key`/`confirm-before`
/// store their bound/wrapped command(s) as unresolved [`RawCmd`]s (tmux does
/// late binding too — the tail is re-parsed against the table at execution
/// time, not here).
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedCmd {
    SplitWindow { horizontal: bool, target: Option<String> },
    SelectPane { dir: Option<Direction>, target: Option<String> },
    SelectWindow { target: String },
    NextWindow,
    PreviousWindow,
    LastWindow,
    LastPane,
    NewWindow { name: Option<String> },
    KillPane { target: Option<String> },
    KillWindow { target: Option<String> },
    ResizePane { dir: Option<Direction>, zoom: bool, count: i32 },
    RenameWindow { target: Option<String>, name: String },
    RenameSession { target: Option<String>, name: String },
    /// `detach-client [-s target]`. `target: None` = detach the ACTING
    /// client (tmux's bare `detach-client`); `Some(s)` = detach every client
    /// attached to session `s`. The Task 6 dispatcher rejects the bare form
    /// when there is no acting client (CLI/config context) with the SP2
    /// verbatim usage error -- `resolve` itself accepts both forms.
    DetachClient { target: Option<String> },
    SendKeys { literal: bool, target: Option<String>, keys: Vec<String> },
    SendPrefix,
    /// `switch-client -p`/`-n` (previous/next session). SP3 only supports
    /// these two flags (documented deviation: tmux's `-l` "last session" is
    /// not tracked in SP3 -- passing `-l` is a `usage:` error like any other
    /// unrecognized flag).
    SwitchClient { next: bool },
    DisplayMessage { text: Option<String> },
    ConfirmBefore { prompt: Option<String>, tail: Vec<RawCmd> },
    CommandPrompt { initial: Option<String> },
    SetOption { global: bool, window: bool, append: bool, unset: bool, name: String, value: Option<String> },
    ShowOptions { global: bool, name: Option<String> },
    BindKey { table: String, repeat: bool, key: String, tail: Vec<RawCmd> },
    UnbindKey { all: bool, table: String, key: Option<String> },
    ListKeys,
    SourceFile { path: String },
    // SP2 CLI commands, folded into the same table.
    NewSession { detached: bool, name: Option<String>, cols: Option<u16>, rows: Option<u16> },
    AttachSession { target: Option<String>, detach_others: bool },
    ListSessions,
    ListWindows { target: Option<String> },
    HasSession { target: String },
    KillSession { target: Option<String> },
    KillServer,
}

/// Map a command name OR any tmux alias to its canonical full name. `None`
/// for an unrecognized name.
fn canonical(name: &str) -> Option<&'static str> {
    Some(match name {
        "split-window" | "splitw" => "split-window",
        "select-pane" | "selectp" => "select-pane",
        "select-window" | "selectw" => "select-window",
        "next-window" | "next" => "next-window",
        "previous-window" | "prev" => "previous-window",
        "last-window" | "last" => "last-window",
        "last-pane" | "lastp" => "last-pane",
        "new-window" | "neww" => "new-window",
        "kill-pane" | "killp" => "kill-pane",
        "kill-window" | "killw" => "kill-window",
        "resize-pane" | "resizep" => "resize-pane",
        "rename-window" | "renamew" => "rename-window",
        "rename-session" | "rename" => "rename-session",
        "detach-client" => "detach-client",
        "send-keys" | "send" => "send-keys",
        "send-prefix" => "send-prefix",
        "switch-client" | "switchc" => "switch-client",
        "display-message" | "display" => "display-message",
        "confirm-before" | "confirm" => "confirm-before",
        "command-prompt" => "command-prompt",
        "set-option" | "set" => "set-option",
        "show-options" | "show" => "show-options",
        "bind-key" | "bind" => "bind-key",
        "unbind-key" | "unbind" => "unbind-key",
        "list-keys" | "lsk" => "list-keys",
        "source-file" | "source" => "source-file",
        "new-session" | "new" => "new-session",
        "attach-session" | "attach" | "a" => "attach-session",
        "list-sessions" | "ls" => "list-sessions",
        "list-windows" | "lsw" => "list-windows",
        "has-session" | "has" => "has-session",
        "kill-session" => "kill-session",
        "kill-server" => "kill-server",
        _ => return None,
    })
}

/// The `usage: ...` line for a command (looked up by full name or alias),
/// `None` if the name is unknown. For the commands SP2's `cli_exec.rs`
/// already implements (`new-session`, `has-session`, `kill-session`,
/// `kill-server`, `rename-session`, `rename-window`, `list-sessions`,
/// `list-windows`, `detach-client`) these strings are copied VERBATIM from
/// that module's `USAGE_*` constants so its existing tests keep passing
/// once the server rewires onto this table.
pub fn usage(name: &str) -> Option<&'static str> {
    let canon = canonical(name)?;
    Some(match canon {
        "split-window" => "usage: split-window [-h] [-v] [-t target]",
        "select-pane" => "usage: select-pane [-L] [-R] [-U] [-D] [-t target]",
        "select-window" => "usage: select-window -t target",
        "next-window" => "usage: next-window",
        "previous-window" => "usage: previous-window",
        "last-window" => "usage: last-window",
        "last-pane" => "usage: last-pane",
        "new-window" => "usage: new-window [-n name]",
        "kill-pane" => "usage: kill-pane [-t target]",
        "kill-window" => "usage: kill-window [-t target]",
        "resize-pane" => "usage: resize-pane [-L] [-R] [-U] [-D] [-Z] [count]",
        "rename-window" => "usage: rename-window [-t target] new-name",
        "rename-session" => "usage: rename-session [-t target] new-name",
        "detach-client" => "usage: detach-client -s target",
        "send-keys" => "usage: send-keys [-l] [-t target] key ...",
        "send-prefix" => "usage: send-prefix",
        "switch-client" => "usage: switch-client [-p] [-n]",
        "display-message" => "usage: display-message [text]",
        "confirm-before" => "usage: confirm-before [-p prompt] command ...",
        "command-prompt" => "usage: command-prompt [-I initial]",
        "set-option" => "usage: set-option [-g] [-w] [-a] [-u] option [value]",
        "show-options" => "usage: show-options [-g] [option]",
        "bind-key" => "usage: bind-key [-n] [-r] [-T table] key command ...",
        "unbind-key" => "usage: unbind-key [-a] [-n] [-T table] [key]",
        "list-keys" => "usage: list-keys",
        "source-file" => "usage: source-file path",
        "new-session" => "usage: new-session [-d] [-s name] [-x cols] [-y rows]",
        "attach-session" => "usage: attach-session [-d] [-t target]",
        "list-sessions" => "usage: list-sessions",
        "list-windows" => "usage: list-windows [-t target]",
        "has-session" => "usage: has-session -t target",
        "kill-session" => "usage: kill-session [-t target]",
        "kill-server" => "usage: kill-server",
        _ => unreachable!("canonical() and usage() command lists diverged"),
    })
}

/// Single-pass flag scanner shared by most `resolve()` arms: `bools` lists
/// no-value flags (e.g. `-h`), `values` lists flags that consume the next
/// token as their value (e.g. `-t`). Any other `-`-prefixed token (length >
/// 1) is `Err(())`. Returns `(bool flags seen, (flag, value) pairs seen,
/// positional tokens in order)`.
type ScanResult = (Vec<String>, Vec<(String, String)>, Vec<String>);

fn scan_flags(args: &[String], bools: &[&str], values: &[&str]) -> Result<ScanResult, ()> {
    let mut bool_hits = Vec::new();
    let mut value_hits = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        if tok.len() > 1 && tok.starts_with('-') {
            if bools.contains(&tok) {
                bool_hits.push(tok.to_string());
                i += 1;
            } else if values.contains(&tok) {
                i += 1;
                let v = args.get(i).ok_or(())?.clone();
                value_hits.push((tok.to_string(), v));
                i += 1;
            } else {
                return Err(());
            }
        } else {
            positional.push(tok.to_string());
            i += 1;
        }
    }
    Ok((bool_hits, value_hits, positional))
}

fn has(hits: &[String], flag: &str) -> bool {
    hits.iter().any(|h| h == flag)
}

fn value_of(hits: &[(String, String)], flag: &str) -> Option<String> {
    hits.iter().find(|(f, _)| f == flag).map(|(_, v)| v.clone())
}

/// Split a `bind-key`/`confirm-before` tail token list into `RawCmd`s on
/// every exact `;` token (whether that token reached here via an escaped
/// `\;` or an equivalent quoted `";"`/`';'` — both are indistinguishable
/// bare `;` tokens by the time `parse_line` is done, and tmux treats them
/// the same way here). Empty runs between separators (leading/trailing/
/// doubled `;`) are dropped, not emitted as empty commands.
fn split_tail(tokens: &[String]) -> Vec<RawCmd> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for t in tokens {
        if t == ";" {
            if let Some(name) = cur.first().cloned() {
                out.push(RawCmd { name, args: cur[1..].to_vec() });
            }
            cur.clear();
        } else {
            cur.push(t.clone());
        }
    }
    if let Some(name) = cur.first().cloned() {
        out.push(RawCmd { name, args: cur[1..].to_vec() });
    }
    out
}

fn direction_of(hits: &[String]) -> Option<Direction> {
    if has(hits, "-L") {
        Some(Direction::Left)
    } else if has(hits, "-R") {
        Some(Direction::Right)
    } else if has(hits, "-U") {
        Some(Direction::Up)
    } else if has(hits, "-D") {
        Some(Direction::Down)
    } else {
        None
    }
}

/// Resolve one [`RawCmd`] (full name or alias) into a typed [`ParsedCmd`] via
/// the command table. `Err("unknown command: <name>")` for an unrecognized
/// name; `Err("usage: <usage line>")` for a bad/missing flag or argument —
/// see [`usage`] for the exact per-command strings (several are verbatim
/// copies of SP2's `cli_exec.rs` usage constants; see that fn's doc comment).
pub fn resolve(raw: &RawCmd) -> Result<ParsedCmd, String> {
    let Some(canon) = canonical(&raw.name) else {
        return Err(format!("unknown command: {}", raw.name));
    };
    let usage_str = usage(canon).expect("canonical() and usage() command lists must agree");
    let bad = || usage_str.to_string();

    match canon {
        "split-window" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-h", "-v"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SplitWindow { horizontal: has(&b, "-h"), target: value_of(&v, "-t") })
        }
        "select-pane" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-L", "-R", "-U", "-D"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SelectPane { dir: direction_of(&b), target: value_of(&v, "-t") })
        }
        "select-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let Some(target) = value_of(&v, "-t") else { return Err(bad()) };
            Ok(ParsedCmd::SelectWindow { target })
        }
        "next-window" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::NextWindow)
        }
        "previous-window" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::PreviousWindow)
        }
        "last-window" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::LastWindow)
        }
        "last-pane" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::LastPane)
        }
        "new-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-n"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::NewWindow { name: value_of(&v, "-n") })
        }
        "kill-pane" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillPane { target: value_of(&v, "-t") })
        }
        "kill-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillWindow { target: value_of(&v, "-t") })
        }
        "resize-pane" => {
            let Ok((b, _, p)) = scan_flags(&raw.args, &["-L", "-R", "-U", "-D", "-Z"], &[]) else { return Err(bad()) };
            if p.len() > 1 {
                return Err(bad());
            }
            let count = match p.first() {
                Some(s) => s.parse::<i32>().map_err(|_| bad())?,
                None => 1,
            };
            Ok(ParsedCmd::ResizePane { dir: direction_of(&b), zoom: has(&b, "-Z"), count })
        }
        "rename-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if p.len() != 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::RenameWindow { target: value_of(&v, "-t"), name: p[0].clone() })
        }
        "rename-session" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if p.len() != 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::RenameSession { target: value_of(&v, "-t"), name: p[0].clone() })
        }
        "detach-client" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-s"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            // -s is optional at the parse level: bare `detach-client`
            // detaches the acting client (tmux behavior; the default
            // prefix-d binding relies on this). The Task 6 dispatcher -- not
            // resolve -- rejects the bare form when no client context
            // exists, with the SP2 verbatim usage error.
            Ok(ParsedCmd::DetachClient { target: value_of(&v, "-s") })
        }
        "send-keys" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-l"], &["-t"]) else { return Err(bad()) };
            if p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SendKeys { literal: has(&b, "-l"), target: value_of(&v, "-t"), keys: p })
        }
        "send-prefix" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SendPrefix)
        }
        "switch-client" => {
            let Ok((b, _, p)) = scan_flags(&raw.args, &["-p", "-n"], &[]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let has_p = has(&b, "-p");
            let has_n = has(&b, "-n");
            if has_p == has_n {
                // Neither given, or both given: SP3 requires exactly one.
                return Err(bad());
            }
            Ok(ParsedCmd::SwitchClient { next: has_n })
        }
        "display-message" => {
            if raw.args.is_empty() {
                Ok(ParsedCmd::DisplayMessage { text: None })
            } else {
                Ok(ParsedCmd::DisplayMessage { text: Some(raw.args.join(" ")) })
            }
        }
        "confirm-before" => {
            let mut prompt: Option<String> = None;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-p" => {
                        i += 1;
                        let Some(p) = raw.args.get(i) else { return Err(bad()) };
                        prompt = Some(p.clone());
                        i += 1;
                    }
                    _ => break,
                }
            }
            let tail_tokens = &raw.args[i..];
            if tail_tokens.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ConfirmBefore { prompt, tail: split_tail(tail_tokens) })
        }
        "command-prompt" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-I"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::CommandPrompt { initial: value_of(&v, "-I") })
        }
        "set-option" => {
            let mut global = false;
            let mut window = false;
            let mut append = false;
            let mut unset = false;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-g" => {
                        global = true;
                        i += 1;
                    }
                    "-w" => {
                        window = true;
                        i += 1;
                    }
                    "-a" => {
                        append = true;
                        i += 1;
                    }
                    "-u" => {
                        unset = true;
                        i += 1;
                    }
                    _ => break,
                }
            }
            let rest = &raw.args[i..];
            let Some((name, value_tokens)) = rest.split_first() else { return Err(bad()) };
            let value = if value_tokens.is_empty() { None } else { Some(value_tokens.join(" ")) };
            Ok(ParsedCmd::SetOption { global, window, append, unset, name: name.clone(), value })
        }
        "show-options" => {
            let Ok((b, _, p)) = scan_flags(&raw.args, &["-g"], &[]) else { return Err(bad()) };
            if p.len() > 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::ShowOptions { global: has(&b, "-g"), name: p.into_iter().next() })
        }
        "bind-key" => {
            let mut table: Option<String> = None;
            let mut repeat = false;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-n" => {
                        table = Some("root".to_string());
                        i += 1;
                    }
                    "-r" => {
                        repeat = true;
                        i += 1;
                    }
                    "-T" => {
                        i += 1;
                        let Some(t) = raw.args.get(i) else { return Err(bad()) };
                        if t != "root" && t != "prefix" {
                            return Err(format!("unknown key table: {t}"));
                        }
                        table = Some(t.clone());
                        i += 1;
                    }
                    _ => break,
                }
            }
            let Some(key) = raw.args.get(i) else { return Err(bad()) };
            let tail_tokens = &raw.args[i + 1..];
            if tail_tokens.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::BindKey {
                table: table.unwrap_or_else(|| "prefix".to_string()),
                repeat,
                key: key.clone(),
                tail: split_tail(tail_tokens),
            })
        }
        "unbind-key" => {
            let mut all = false;
            let mut table: Option<String> = None;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-a" => {
                        all = true;
                        i += 1;
                    }
                    "-n" => {
                        table = Some("root".to_string());
                        i += 1;
                    }
                    "-T" => {
                        i += 1;
                        let Some(t) = raw.args.get(i) else { return Err(bad()) };
                        if t != "root" && t != "prefix" {
                            return Err(format!("unknown key table: {t}"));
                        }
                        table = Some(t.clone());
                        i += 1;
                    }
                    _ => break,
                }
            }
            let rest = &raw.args[i..];
            if rest.len() > 1 {
                return Err(bad());
            }
            let key = rest.first().cloned();
            if !all && key.is_none() {
                return Err(bad());
            }
            Ok(ParsedCmd::UnbindKey { all, table: table.unwrap_or_else(|| "prefix".to_string()), key })
        }
        "list-keys" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListKeys)
        }
        "source-file" => match raw.args.as_slice() {
            [path] => Ok(ParsedCmd::SourceFile { path: path.clone() }),
            _ => Err(bad()),
        },
        "new-session" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-d"], &["-s", "-x", "-y"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let cols = match value_of(&v, "-x") {
                Some(s) => Some(s.parse::<u16>().map_err(|_| bad())?),
                None => None,
            };
            let rows = match value_of(&v, "-y") {
                Some(s) => Some(s.parse::<u16>().map_err(|_| bad())?),
                None => None,
            };
            Ok(ParsedCmd::NewSession { detached: has(&b, "-d"), name: value_of(&v, "-s"), cols, rows })
        }
        "attach-session" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-d"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::AttachSession { target: value_of(&v, "-t"), detach_others: has(&b, "-d") })
        }
        "list-sessions" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListSessions)
        }
        "list-windows" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListWindows { target: value_of(&v, "-t") })
        }
        "has-session" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let Some(target) = value_of(&v, "-t") else { return Err(bad()) };
            Ok(ParsedCmd::HasSession { target })
        }
        "kill-session" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillSession { target: value_of(&v, "-t") })
        }
        "kill-server" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillServer)
        }
        _ => unreachable!("canonical() and resolve() command lists diverged"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, args: &[&str]) -> RawCmd {
        RawCmd { name: name.to_string(), args: args.iter().map(|s| s.to_string()).collect() }
    }

    // ---- tokenizer ----

    #[test]
    fn plain_split() {
        assert_eq!(parse_line("split-window -h -t work").unwrap(), vec![raw("split-window", &["-h", "-t", "work"])]);
    }

    #[test]
    fn single_quotes_literal() {
        // '#' and ';' inside single quotes are literal, not comment/separator.
        assert_eq!(parse_line("echo 'a # b ; c' rest").unwrap(), vec![raw("echo", &["a # b ; c", "rest"])]);
    }

    #[test]
    fn double_quotes_escapes() {
        assert_eq!(parse_line(r#"echo "a\"b\\c""#).unwrap(), vec![raw("echo", &[r#"a"b\c"#])]);
    }

    #[test]
    fn double_quotes_other_backslash_is_literal() {
        // \n inside double quotes is NOT a newline escape -- backslash and
        // the following char both survive verbatim (only \" and \\ are
        // recognized escapes).
        assert_eq!(parse_line(r#"echo "a\nb""#).unwrap(), vec![raw("echo", &[r"a\nb"])]);
    }

    #[test]
    fn comment_strips() {
        assert_eq!(parse_line("set -g status off # trailing").unwrap(), vec![raw("set", &["-g", "status", "off"])]);
    }

    #[test]
    fn comment_strips_mid_token() {
        assert_eq!(parse_line("foo#bar").unwrap(), vec![raw("foo", &[])]);
    }

    #[test]
    fn semicolon_splits_commands() {
        assert_eq!(
            parse_line("kill-pane ; display-message hi").unwrap(),
            vec![raw("kill-pane", &[]), raw("display-message", &["hi"])]
        );
    }

    #[test]
    fn escaped_semicolon_is_arg() {
        let cmds = parse_line(r"bind x kill-pane \; display ok").unwrap();
        assert_eq!(cmds, vec![raw("bind", &["x", "kill-pane", ";", "display", "ok"])]);
        // ... and resolve()'s tail-splitting turns it into 2 RawCmds.
        let ParsedCmd::BindKey { tail, .. } = resolve(&cmds[0]).unwrap() else { panic!("expected BindKey") };
        assert_eq!(tail, vec![raw("kill-pane", &[]), raw("display", &["ok"])]);
    }

    #[test]
    fn quoted_semicolon_also_splits_tail() {
        let cmds = parse_line(r#"bind x kill-pane ";" display ok"#).unwrap();
        let ParsedCmd::BindKey { tail, .. } = resolve(&cmds[0]).unwrap() else { panic!("expected BindKey") };
        assert_eq!(tail, vec![raw("kill-pane", &[]), raw("display", &["ok"])]);
    }

    #[test]
    fn unterminated_quote_err() {
        assert!(parse_line("\"foo").unwrap_err().contains("unterminated quote"));
        assert!(parse_line("'foo").unwrap_err().contains("unterminated quote"));
    }

    #[test]
    fn blank_and_comment_only_lines_are_empty() {
        assert_eq!(parse_line("").unwrap(), vec![]);
        assert_eq!(parse_line("   ").unwrap(), vec![]);
        assert_eq!(parse_line("# just a comment").unwrap(), vec![]);
        assert_eq!(parse_line("a ; ; b").unwrap(), vec![raw("a", &[]), raw("b", &[])]);
    }

    #[test]
    fn adjacent_quote_concatenation() {
        assert_eq!(parse_line(r#"foo'bar'"baz""#).unwrap(), vec![raw("foobarbaz", &[])]);
    }

    // ---- join_continuations ----

    #[test]
    fn join_continuations_passthrough() {
        assert_eq!(
            join_continuations(["a", "b"].into_iter()),
            vec![(1, "a".to_string()), (2, "b".to_string())]
        );
    }

    #[test]
    fn join_continuations_basic() {
        assert_eq!(join_continuations([r"foo \", "bar"].into_iter()), vec![(1, "foo bar".to_string())]);
    }

    #[test]
    fn join_continuations_chain_tracks_first_line_number() {
        assert_eq!(
            join_continuations(["x", r"a \", r"b \", "c"].into_iter()),
            vec![(1, "x".to_string()), (2, "a b c".to_string())]
        );
    }

    #[test]
    fn join_continuations_trailing_backslash_at_eof_kept() {
        assert_eq!(join_continuations([r"foo\"].into_iter()), vec![(1, r"foo\".to_string())]);
    }

    #[test]
    fn join_continuations_strips_crlf() {
        assert_eq!(join_continuations(["foo \\\r", "bar\r"].into_iter()), vec![(1, "foo bar".to_string())]);
    }

    // ---- resolve: aliases ----

    #[test]
    fn aliases_resolve() {
        assert_eq!(
            resolve(&raw("splitw", &["-h"])).unwrap(),
            resolve(&raw("split-window", &["-h"])).unwrap()
        );
        assert_eq!(
            resolve(&raw("set", &["-g", "status", "off"])).unwrap(),
            resolve(&raw("set-option", &["-g", "status", "off"])).unwrap()
        );
    }

    #[test]
    fn split_window_flags() {
        assert_eq!(
            resolve(&raw("split-window", &["-h", "-t", "work"])).unwrap(),
            ParsedCmd::SplitWindow { horizontal: true, target: Some("work".to_string()) }
        );
        assert_eq!(
            resolve(&raw("split-window", &["-v"])).unwrap(),
            ParsedCmd::SplitWindow { horizontal: false, target: None }
        );
    }

    #[test]
    fn send_keys_literal_flag() {
        assert_eq!(
            resolve(&raw("send-keys", &["-l", "-t", "work", "hello"])).unwrap(),
            ParsedCmd::SendKeys { literal: true, target: Some("work".to_string()), keys: vec!["hello".to_string()] }
        );
    }

    #[test]
    fn switch_client_prev_and_next() {
        assert_eq!(resolve(&raw("switch-client", &["-p"])).unwrap(), ParsedCmd::SwitchClient { next: false });
        assert_eq!(resolve(&raw("switchc", &["-n"])).unwrap(), ParsedCmd::SwitchClient { next: true });
    }

    #[test]
    fn switch_client_requires_exactly_one_of_p_n() {
        assert_eq!(resolve(&raw("switch-client", &[])), Err(usage("switch-client").unwrap().to_string()));
        assert_eq!(resolve(&raw("switch-client", &["-p", "-n"])), Err(usage("switch-client").unwrap().to_string()));
    }

    #[test]
    fn switch_client_dash_l_is_usage_error() {
        // SP3 deviation: -l ("last session") is not supported; it hits the
        // same usage: error as any other unrecognized flag.
        assert_eq!(resolve(&raw("switch-client", &["-l"])), Err(usage("switch-client").unwrap().to_string()));
    }

    #[test]
    fn send_keys_requires_a_key() {
        assert_eq!(resolve(&raw("send-keys", &["-t", "work"])).unwrap_err(), usage("send-keys").unwrap());
    }

    #[test]
    fn set_option_flags() {
        assert_eq!(
            resolve(&raw("set-option", &["-g", "-a", "status-left", "[#S] "])).unwrap(),
            ParsedCmd::SetOption {
                global: true,
                window: false,
                append: true,
                unset: false,
                name: "status-left".to_string(),
                value: Some("[#S] ".to_string()),
            }
        );
        // Multiple bare value tokens are joined with single spaces.
        assert_eq!(
            resolve(&raw("set", &["-g", "status-left", "a", "b", "c"])).unwrap(),
            ParsedCmd::SetOption {
                global: true,
                window: false,
                append: false,
                unset: false,
                name: "status-left".to_string(),
                value: Some("a b c".to_string()),
            }
        );
        // No value at all is allowed (semantics decided by `options` later).
        assert_eq!(
            resolve(&raw("set", &["-u", "mouse"])).unwrap(),
            ParsedCmd::SetOption {
                global: false,
                window: false,
                append: false,
                unset: true,
                name: "mouse".to_string(),
                value: None,
            }
        );
    }

    #[test]
    fn bind_key_full() {
        assert_eq!(
            resolve(&raw("bind", &["-r", "-T", "prefix", "C-Up", "resize-pane", "-U"])).unwrap(),
            ParsedCmd::BindKey {
                table: "prefix".to_string(),
                repeat: true,
                key: "C-Up".to_string(),
                tail: vec![raw("resize-pane", &["-U"])],
            }
        );
    }

    #[test]
    fn bind_key_dash_n_is_root_table() {
        let ParsedCmd::BindKey { table, .. } = resolve(&raw("bind", &["-n", "F5", "select-pane", "-U"])).unwrap()
        else {
            panic!("expected BindKey")
        };
        assert_eq!(table, "root");
    }

    #[test]
    fn bind_key_bad_table_errs() {
        assert_eq!(
            resolve(&raw("bind", &["-T", "other", "x", "kill-pane"])).unwrap_err(),
            "unknown key table: other"
        );
    }

    #[test]
    fn unbind_key_table_validation() {
        assert_eq!(
            resolve(&raw("unbind-key", &["-T", "custom", "x"])).unwrap_err(),
            "unknown key table: custom"
        );
        assert_eq!(
            resolve(&raw("unbind-key", &["-a"])).unwrap(),
            ParsedCmd::UnbindKey { all: true, table: "prefix".to_string(), key: None }
        );
        assert_eq!(resolve(&raw("unbind-key", &[])).unwrap_err(), usage("unbind-key").unwrap());
    }

    #[test]
    fn confirm_before_tail() {
        assert_eq!(
            resolve(&raw("confirm", &["-p", "kill-pane #P? (y/n)", "kill-pane"])).unwrap(),
            ParsedCmd::ConfirmBefore {
                prompt: Some("kill-pane #P? (y/n)".to_string()),
                tail: vec![raw("kill-pane", &[])],
            }
        );
    }

    #[test]
    fn resize_pane_defaults_and_errors() {
        assert_eq!(
            resolve(&raw("resize-pane", &["-U"])).unwrap(),
            ParsedCmd::ResizePane { dir: Some(Direction::Up), zoom: false, count: 1 }
        );
        assert_eq!(
            resolve(&raw("resize-pane", &["-L", "5"])).unwrap(),
            ParsedCmd::ResizePane { dir: Some(Direction::Left), zoom: false, count: 5 }
        );
        assert_eq!(resolve(&raw("resize-pane", &["-L", "abc"])).unwrap_err(), usage("resize-pane").unwrap());
    }

    #[test]
    fn unknown_command_err_exact() {
        assert_eq!(resolve(&raw("bogus-cmd", &[])).unwrap_err(), "unknown command: bogus-cmd");
    }

    #[test]
    fn usage_err_on_bad_flag() {
        assert_eq!(
            resolve(&raw("rename-session", &["-q", "foo"])).unwrap_err(),
            "usage: rename-session [-t target] new-name"
        );
    }

    #[test]
    fn sp2_commands_present() {
        assert_eq!(
            resolve(&raw("new-session", &["-s", "work"])).unwrap(),
            ParsedCmd::NewSession { detached: false, name: Some("work".to_string()), cols: None, rows: None }
        );
        assert_eq!(resolve(&raw("ls", &[])).unwrap(), ParsedCmd::ListSessions);
        assert_eq!(resolve(&raw("kill-server", &[])).unwrap(), ParsedCmd::KillServer);
        assert_eq!(
            resolve(&raw("has-session", &["-t", "work"])).unwrap(),
            ParsedCmd::HasSession { target: "work".to_string() }
        );
        assert_eq!(
            resolve(&raw("detach-client", &["-s", "work"])).unwrap(),
            ParsedCmd::DetachClient { target: Some("work".to_string()) }
        );
    }

    #[test]
    fn detach_client_bare_is_current_client() {
        // Bare `detach-client` (tmux: detach the acting client) resolves;
        // the "no client context" rejection is the dispatcher's job (Task
        // 6), not resolve's. The default prefix-d binding depends on this.
        assert_eq!(resolve(&raw("detach-client", &[])).unwrap(), ParsedCmd::DetachClient { target: None });
    }

    #[test]
    fn sp2_usage_strings_match_cli_exec_verbatim() {
        // These MUST stay byte-for-byte identical to src/server/cli_exec.rs's
        // USAGE_* constants (contract requirement, SP2 test parity).
        assert_eq!(usage("list-sessions").unwrap(), "usage: list-sessions");
        assert_eq!(usage("has-session").unwrap(), "usage: has-session -t target");
        assert_eq!(usage("kill-session").unwrap(), "usage: kill-session [-t target]");
        assert_eq!(usage("kill-server").unwrap(), "usage: kill-server");
        assert_eq!(usage("new-session").unwrap(), "usage: new-session [-d] [-s name] [-x cols] [-y rows]");
        assert_eq!(usage("rename-session").unwrap(), "usage: rename-session [-t target] new-name");
        assert_eq!(usage("rename-window").unwrap(), "usage: rename-window [-t target] new-name");
        assert_eq!(usage("list-windows").unwrap(), "usage: list-windows [-t target]");
        assert_eq!(usage("detach-client").unwrap(), "usage: detach-client -s target");
    }

    #[test]
    fn usage_lookup_by_alias_and_unknown() {
        assert_eq!(usage("splitw"), usage("split-window"));
        assert_eq!(usage("nonexistent-command"), None);
    }

    #[test]
    fn rename_window_requires_name() {
        assert_eq!(
            resolve(&raw("rename-window", &["-t", "foo"])).unwrap_err(),
            usage("rename-window").unwrap()
        );
        assert_eq!(
            resolve(&raw("renamew", &["newname"])).unwrap(),
            ParsedCmd::RenameWindow { target: None, name: "newname".to_string() }
        );
    }

    #[test]
    fn show_options_and_display_message() {
        assert_eq!(
            resolve(&raw("show", &["-g", "status-left"])).unwrap(),
            ParsedCmd::ShowOptions { global: true, name: Some("status-left".to_string()) }
        );
        assert_eq!(resolve(&raw("show", &[])).unwrap(), ParsedCmd::ShowOptions { global: false, name: None });
        assert_eq!(
            resolve(&raw("display", &["hello", "world"])).unwrap(),
            ParsedCmd::DisplayMessage { text: Some("hello world".to_string()) }
        );
        assert_eq!(resolve(&raw("display-message", &[])).unwrap(), ParsedCmd::DisplayMessage { text: None });
    }
}
