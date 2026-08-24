//! Asks the terminal what graphics protocol it actually supports, and reports it.
//!
//! This has to run in a *real* terminal: the query is an escape sequence and the answer
//! comes back from the terminal emulator, so a pipe or a bare pty proves nothing.
//!
//! Exists because the evidence disagreed. `terminfo.dev` reports Ghostty 1.3.1 as failing
//! Unicode placeholders, which contradicts both Ghostty's own source and `ratatui-image`'s
//! QA. Rather than pick a side, ask the terminal.
//!
//! ```console
//! $ cargo run --example graphics_probe
//! ```

use ratatui_image::picker::{Picker, ProtocolType};

fn main() {
    println!("TERM              {}", env("TERM"));
    println!("TERM_PROGRAM      {}", env("TERM_PROGRAM"));
    println!("TMUX              {}", env("TMUX"));
    println!(
        "inside tmux       {}",
        if std::env::var_os("TMUX").is_some() {
            "yes -- queries must pass through DCS passthrough"
        } else {
            "no"
        }
    );
    println!();

    // `from_query_stdio` puts the terminal in raw mode, writes the capability queries, and
    // reads the replies. Under tmux, ratatui-image wraps them in DCS passthrough and turns
    // `allow-passthrough` on itself.
    match Picker::from_query_stdio() {
        Ok(picker) => {
            let protocol = picker.protocol_type();
            println!("protocol          {protocol:?}");
            println!("font size (px)    {:?}", picker.font_size());

            let verdict = match protocol {
                ProtocolType::Kitty => {
                    "Kitty graphics available. The pixel path can use it, and Unicode \
                     placeholders are the mechanism that survives tmux."
                }
                ProtocolType::Sixel => {
                    "Sixel only. Workable, but no Unicode placeholders, so tmux pane \
                     switching and clipping are on the terminal rather than on tmux."
                }
                ProtocolType::Iterm2 => "iTerm2 protocol. Not what Ghostty advertises.",
                ProtocolType::Halfblocks => {
                    "NO graphics protocol detected -- falling back to half blocks. The \
                     pixel path would render as coloured cells, not pixels."
                }
            };
            println!("\nverdict           {verdict}");
        }
        Err(err) => {
            println!("query FAILED      {err}");
            println!(
                "\nverdict           Could not query the terminal. Run this in a real \
                 terminal, not through a pipe."
            );
        }
    }
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| "<unset>".to_owned())
}
